//! Startup-only settings. Files are parsed as bounded data, never sourced as
//! shell scripts, and process environment variables are never mutated.

use bytes::Bytes;
use recall_server::Config;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;

const MAX_ENV_BYTES: usize = 16 * 1024;
const MAX_ENV_LINE_BYTES: usize = 2048;
const ENV_NAMES: &[&str] = &[
    "RECALL_BIND",
    "RECALL_WORKERS",
    "RECALL_IO_THREADS",
    "RECALL_MAX_MEMORY_MIB",
    "RECALL_KEYS_PER_WORKER",
    "RECALL_MAX_CONNECTIONS",
    "RECALL_QUEUE_BYTES_MIB",
    "RECALL_PASSWORD",
];

type Values = BTreeMap<String, String>;

struct Options {
    env_file: Option<PathBuf>,
    explicit_file: bool,
    values: Values,
}

pub fn load(arguments: &[String]) -> Result<Config, String> {
    let options = parse_options(arguments)?;
    let mut process = Values::new();
    for (key, value) in std::env::vars_os() {
        let Some(key) = key.to_str().filter(|key| key.starts_with("RECALL_")) else {
            continue;
        };
        validate_name(key)?;
        let value = value
            .into_string()
            .map_err(|_| format!("{key} must be valid UTF-8"))?;
        if value.len() > MAX_ENV_LINE_BYTES {
            return Err(format!("{key} exceeds the configuration value limit"));
        }
        process.insert(key.to_owned(), value);
    }
    let file = match options.env_file {
        Some(path) => match File::open(&path) {
            Ok(file) => {
                if !file.metadata().map_err(|_| "cannot inspect environment file")?.is_file() {
                    return Err("environment path must be a regular file".to_owned());
                }
                read_values(file)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && !options.explicit_file => Values::new(),
            Err(error) => return Err(format!("cannot read environment file: {}", error.kind())),
        },
        None => Values::new(),
    };
    from_sources(file, process, options.values)
}

fn parse_options(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        env_file: Some(PathBuf::from(".env")),
        explicit_file: false,
        values: Values::new(),
    };
    let mut file_option_seen = false;
    let mut arguments = arguments.iter();
    while let Some(option) = arguments.next() {
        if option == "--no-env-file" {
            if file_option_seen {
                return Err("choose only one environment-file option".to_owned());
            }
            file_option_seen = true;
            options.env_file = None;
            continue;
        }
        if option == "--env-file" {
            if file_option_seen {
                return Err("choose only one environment-file option".to_owned());
            }
            let path = arguments.next().ok_or("--env-file requires a path")?;
            if path.is_empty() {
                return Err("--env-file requires a nonempty path".to_owned());
            }
            file_option_seen = true;
            options.explicit_file = true;
            options.env_file = Some(PathBuf::from(path));
            continue;
        }
        let name = match option.as_str() {
            "--bind" => "RECALL_BIND",
            "--workers" => "RECALL_WORKERS",
            "--io-threads" => "RECALL_IO_THREADS",
            "--max-memory-mib" => "RECALL_MAX_MEMORY_MIB",
            "--keys-per-worker" => "RECALL_KEYS_PER_WORKER",
            "--max-connections" => "RECALL_MAX_CONNECTIONS",
            "--queue-bytes-mib" => "RECALL_QUEUE_BYTES_MIB",
            _ => return Err("unknown option; use --help".to_owned()),
        };
        let value = arguments.next().ok_or_else(|| format!("missing value for {option}"))?;
        if options.values.insert(name.to_owned(), value.to_owned()).is_some() {
            return Err(format!("duplicate option {option}"));
        }
    }
    Ok(options)
}

fn read_values(reader: impl Read) -> Result<Values, String> {
    let mut input = Vec::new();
    reader.take((MAX_ENV_BYTES + 1) as u64).read_to_end(&mut input)
        .map_err(|_| "cannot read environment file".to_owned())?;
    if input.len() > MAX_ENV_BYTES {
        return Err("environment file exceeds 16 KiB".to_owned());
    }
    let text = std::str::from_utf8(&input).map_err(|_| "environment file must be UTF-8".to_owned())?;
    parse_values(text)
}

fn parse_values(text: &str) -> Result<Values, String> {
    let mut values = Values::new();
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let invalid = || format!("invalid environment assignment at line {}", index + 1);
        if line.len() > MAX_ENV_LINE_BYTES || line.contains('\0') {
            return Err(invalid());
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw_value) = line.split_once('=').ok_or_else(invalid)?;
        let key = key.trim();
        validate_name(key).map_err(|_| invalid())?;
        let raw_value = raw_value.trim();
        let value = if raw_value.starts_with('\'') || raw_value.starts_with('"') {
            let quote = raw_value.as_bytes()[0];
            if raw_value.len() < 2 || raw_value.as_bytes().last() != Some(&quote) {
                return Err(invalid());
            }
            &raw_value[1..raw_value.len() - 1]
        } else {
            raw_value
        };
        if values.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(format!("duplicate environment assignment at line {}", index + 1));
        }
    }
    Ok(values)
}

fn validate_name(key: &str) -> Result<(), String> {
    if ENV_NAMES.contains(&key) {
        Ok(())
    } else {
        // Do not echo arbitrary input: diagnostics must not expose secrets.
        Err("unknown Recall environment setting".to_owned())
    }
}

fn from_sources(mut file: Values, process: Values, arguments: Values) -> Result<Config, String> {
    file.extend(process);
    file.extend(arguments);
    let mut config = Config::default();
    for (key, value) in file {
        match key.as_str() {
            "RECALL_BIND" => config.bind = value.parse().map_err(|_| "RECALL_BIND requires an IP:port")?,
            "RECALL_WORKERS" => config.workers = number(&key, &value)?,
            "RECALL_IO_THREADS" => config.io_threads = number(&key, &value)?,
            "RECALL_MAX_MEMORY_MIB" => config.max_payload_bytes = mib(&key, &value)?,
            "RECALL_KEYS_PER_WORKER" => config.keys_per_worker = number(&key, &value)?,
            "RECALL_MAX_CONNECTIONS" => config.max_connections = number(&key, &value)?,
            "RECALL_QUEUE_BYTES_MIB" => config.queue_bytes = mib(&key, &value)?,
            "RECALL_PASSWORD" => config.password = Some(Bytes::from(value)),
            _ => return Err("unknown Recall environment setting".to_owned()),
        }
    }
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

fn number(key: &str, value: &str) -> Result<usize, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{key} requires a nonnegative decimal integer"));
    }
    value.parse().map_err(|_| format!("{key} is out of range"))
}

fn mib(key: &str, value: &str) -> Result<usize, String> {
    number(key, value)?.checked_mul(1024 * 1024).ok_or_else(|| format!("{key} is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_arguments_then_environment_then_file_then_defaults() {
        let file = parse_values("RECALL_WORKERS=1\nRECALL_IO_THREADS=1\nRECALL_MAX_CONNECTIONS=10").unwrap();
        let process = parse_values("RECALL_WORKERS=2\nRECALL_IO_THREADS=2").unwrap();
        let arguments = parse_values("RECALL_WORKERS=3").unwrap();
        let config = from_sources(file, process, arguments).unwrap();
        assert_eq!(config.workers, 3);
        assert_eq!(config.io_threads, 2);
        assert_eq!(config.max_connections, 10);
        assert_eq!(config.queue_capacity, Config::default().queue_capacity);
    }

    #[test]
    fn parsing_is_literal_and_supports_utf8_bom_crlf_and_quotes() {
        let values = parse_values("\u{feff}# comment\r\nRECALL_PASSWORD='a $HOME # literal'\r\nRECALL_WORKERS=2\r\n").unwrap();
        assert_eq!(values["RECALL_PASSWORD"], "a $HOME # literal");
        assert_eq!(values["RECALL_WORKERS"], "2");
    }

    #[test]
    fn invalid_files_are_rejected_without_leaking_values() {
        for input in [
            "RECALL_PASSWORD='sensitive",
            "RECALL_PASSWORD=sensitive\nRECALL_PASSWORD=other",
            "export RECALL_PASSWORD=sensitive",
            "RECALL_UNKNOWN=sensitive",
            "RECALL_PASSWORD=sensitive\0",
        ] {
            let error = parse_values(input).unwrap_err();
            assert!(!error.contains("sensitive"));
        }
        assert!(read_values(&vec![b'x'; MAX_ENV_BYTES + 1][..]).is_err());
        assert!(read_values(&[0xff][..]).is_err());
    }

    #[test]
    fn loader_preserves_security_and_password_validation() {
        for input in ["RECALL_BIND=0.0.0.0:6379", "RECALL_PASSWORD=", "RECALL_WORKERS=0", "RECALL_MAX_MEMORY_MIB=999999999999999999999999999"] {
            assert!(from_sources(parse_values(input).unwrap(), Values::new(), Values::new()).is_err());
        }
    }

    #[test]
    fn environment_file_selection_is_explicit() {
        let options = parse_options(&["--no-env-file".to_owned(), "--workers".to_owned(), "2".to_owned()]).unwrap();
        assert!(options.env_file.is_none());
        assert_eq!(options.values["RECALL_WORKERS"], "2");
        let options = parse_options(&["--env-file".to_owned(), "local.env".to_owned()]).unwrap();
        assert!(options.explicit_file);
        assert!(parse_options(&["--no-env-file".to_owned(), "--env-file".to_owned(), "local.env".to_owned()]).is_err());
        assert!(parse_options(&["--env-file".to_owned()]).is_err());
    }
}
