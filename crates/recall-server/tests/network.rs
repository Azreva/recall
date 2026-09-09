mod support;

use bytes::Bytes;
use recall_protocol::Reply;
use std::time::Duration;
use support::{command, config, frame, response, TestServer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_binary_frames_and_pipelines_preserve_order() {
    let server = TestServer::start(config()).await;
    let mut stream = server.connect().await;
    let mut pipeline = frame(&[b"SET", b"\0key", b"hello\r\n\0"]);
    pipeline.extend(frame(&[b"GET", b"\0key"]));
    pipeline.extend(frame(&[b"UNKNOWN"]));
    pipeline.extend(frame(&[b"PING"]));
    for fragment in pipeline.chunks(3) {
        stream.write_all(fragment).await.unwrap();
    }
    assert_eq!(response(&mut stream).await, Reply::ok());
    assert_eq!(
        response(&mut stream).await,
        Reply::bulk(Bytes::from_static(b"hello\r\n\0"))
    );
    assert!(matches!(response(&mut stream).await, Reply::Error(_)));
    assert_eq!(
        response(&mut stream).await,
        Reply::Simple(Bytes::from_static(b"PONG"))
    );
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn oversized_declared_length_is_rejected_without_waiting_for_payload() {
    let server = TestServer::start(config()).await;
    let mut stream = server.connect().await;
    stream.write_all(b"*1\r\n$999999999999\r\n").await.unwrap();
    assert!(matches!(response(&mut stream).await, Reply::Error(_)));
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    server.stop().await;
}

#[tokio::test]
async fn authentication_negotiation_and_client_names_do_not_bypass_security() {
    let mut config = config();
    config.password = Some(Bytes::from_static(b"test-secret"));
    let server = TestServer::start(config).await;
    let mut stream = server.connect().await;
    assert_eq!(
        command(&mut stream, &[b"GET", b"k"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    assert_eq!(
        command(
            &mut stream,
            &[b"HELLO", b"3", b"AUTH", b"default", b"test-secret"]
        )
        .await,
        Reply::error("NOPROTO unsupported protocol version")
    );
    assert_eq!(
        command(&mut stream, &[b"GET", b"k"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    assert!(matches!(
        command(&mut stream, &[b"AUTH", b"wrong"]).await,
        Reply::Error(_)
    ));
    assert!(matches!(
        command(
            &mut stream,
            &[
                b"HELLO",
                b"2",
                b"AUTH",
                b"default",
                b"test-secret",
                b"SETNAME",
                b"test-client"
            ]
        )
        .await,
        Reply::Array(_)
    ));
    assert_eq!(
        command(&mut stream, &[b"CLIENT", b"GETNAME"]).await,
        Reply::bulk("test-client")
    );
    assert_eq!(
        command(&mut stream, &[b"CLIENT", b"SETINFO", b"LIB-NAME", b"test"]).await,
        Reply::ok()
    );
    assert!(matches!(
        command(&mut stream, &[b"SELECT", b"1"]).await,
        Reply::Error(_)
    ));
    assert_eq!(command(&mut stream, &[b"SELECT", b"0"]).await, Reply::ok());
    assert_eq!(command(&mut stream, &[b"QUIT"]).await, Reply::ok());
    server.stop().await;
}

/// Authentication is scoped to each connection: a valid login on one connection
/// must not authorize other connections, and new connections start unauthenticated.
#[tokio::test]
async fn authentication_state_is_scoped_per_connection() {
    let mut config = config();
    config.password = Some(Bytes::from_static(b"scoped-secret"));
    let server = TestServer::start(config).await;

    let mut authenticated = server.connect().await;
    let mut unauthenticated = server.connect().await;

    assert_eq!(
        command(&mut unauthenticated, &[b"GET", b"k"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    assert_eq!(
        command(&mut authenticated, &[b"AUTH", b"scoped-secret"]).await,
        Reply::ok()
    );
    assert_eq!(
        command(&mut authenticated, &[b"SET", b"k", b"secret-value"]).await,
        Reply::ok()
    );

    // Other connection remains unauthorized and cannot read the written key.
    assert_eq!(
        command(&mut unauthenticated, &[b"GET", b"k"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    assert_eq!(
        command(&mut unauthenticated, &[b"SET", b"k", b"hijack"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    drop(unauthenticated);

    // A brand-new connection also starts unauthenticated.
    let mut fresh = server.connect().await;
    assert_eq!(
        command(&mut fresh, &[b"GET", b"k"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    assert_eq!(
        command(&mut authenticated, &[b"GET", b"k"]).await,
        Reply::bulk("secret-value")
    );
    drop(fresh);
    drop(authenticated);
    server.stop().await;
}

/// A failed AUTH inside a pipeline must not execute the protected commands that
/// follow it; response order is preserved and rejected writes have no effects.
#[tokio::test]
async fn failed_auth_in_pipeline_does_not_execute_following_writes() {
    let mut config = config();
    config.password = Some(Bytes::from_static(b"pipeline-secret"));
    let server = TestServer::start(config).await;

    let mut stream = server.connect().await;
    let mut pipeline = frame(&[b"AUTH", b"definitely-wrong"]);
    pipeline.extend(frame(&[b"SET", b"pipeline-key", b"smuggled"]));
    pipeline.extend(frame(&[b"PING"]));
    stream.write_all(&pipeline).await.unwrap();
    assert!(matches!(response(&mut stream).await, Reply::Error(_))); // WRONGPASS
    assert_eq!(
        response(&mut stream).await,
        Reply::error("NOAUTH Authentication required.")
    );
    assert_eq!(
        response(&mut stream).await,
        Reply::error("NOAUTH Authentication required.")
    );

    // Confirm on an authenticated connection that the smuggled write never happened.
    assert_eq!(
        command(&mut stream, &[b"AUTH", b"pipeline-secret"]).await,
        Reply::ok()
    );
    assert_eq!(
        command(&mut stream, &[b"GET", b"pipeline-key"]).await,
        Reply::Bulk(None)
    );
    drop(stream);
    server.stop().await;
}

/// Invalid usernames, wrong/empty passwords, and malformed AUTH arguments are all
/// rejected and do not partially authenticate the connection.
#[tokio::test]
async fn invalid_credential_forms_are_rejected_without_side_effects() {
    let mut config = config();
    config.password = Some(Bytes::from_static(b"form-secret"));
    let server = TestServer::start(config).await;

    let mut stream = server.connect().await;
    // Wrong username with correct password.
    assert!(matches!(
        command(&mut stream, &[b"AUTH", b"alice", b"form-secret"]).await,
        Reply::Error(_)
    ));
    // Empty password.
    assert!(matches!(
        command(&mut stream, &[b"AUTH", b""]).await,
        Reply::Error(_)
    ));
    // Malformed argument counts.
    assert!(matches!(
        command(&mut stream, &[b"AUTH"]).await,
        Reply::Error(_)
    ));
    assert!(matches!(
        command(
            &mut stream,
            &[b"AUTH", b"default", b"form-secret", b"extra"]
        )
        .await,
        Reply::Error(_)
    ));
    // Still unauthenticated after all failures.
    assert_eq!(
        command(&mut stream, &[b"GET", b"k"]).await,
        Reply::error("NOAUTH Authentication required.")
    );
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn incomplete_frames_have_an_absolute_deadline_and_shutdown_interrupts_idle_reads() {
    let mut config = config();
    config.read_timeout = Duration::from_millis(100);
    let server = TestServer::start(config).await;
    let mut stream = server.connect().await;
    stream
        .write_all(b"*2\r\n$4\r\nECHO\r\n$100\r\nx")
        .await
        .unwrap();
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(3), stream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    drop(stream);
    let idle = server.connect().await;
    server.stop().await;
    drop(idle);
}
