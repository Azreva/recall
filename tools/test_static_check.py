"""Isolated checker regressions. No Rust, Docker, network, or third-party modules."""

from contextlib import redirect_stdout
from io import StringIO
import json
from pathlib import Path
import tempfile
import unittest

import static_check as check


class StaticCheckTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="recall-static-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def checker(self):
        checker = check.Checker(self.root)
        checker.collect()
        checker.findings.clear()  # These fixtures intentionally isolate rules.
        return checker

    def test_private_environment_and_generated_directories_are_never_read(self):
        for name in (".env", ".env.local", "private.env", "target/ignored.rs", ".git/ignored.toml"):
            self.write(name, "sensitive-value\0")
        self.write(".env.example", "RECALL_WORKERS=2\n")
        checker = self.checker()
        self.assertEqual(set(checker.files), {".env.example"})
        self.assertNotIn("sensitive-value", str(checker.findings))

    def test_python_is_parsed_not_imported(self):
        self.write("sample.py", "raise RuntimeError('must never execute')\n")
        checker = self.checker()
        checker.check_text()
        self.assertEqual(checker.findings, [])
        self.write("sample.py", "def broken(:\n")
        checker = self.checker()
        checker.check_text()
        self.assertTrue(any(item.code == "PYTHON_SYNTAX" for item in checker.findings))

    def test_toml_parse_errors_are_diagnostics(self):
        self.write("broken.toml", "[invalid\n")
        checker = self.checker()
        checker.check_text()
        self.assertEqual(checker.findings[0].code, "TOML_SYNTAX")

    def test_rust_lexical_scan_understands_comments_raw_strings_chars_and_lifetimes(self):
        text = '''fn example<'a>(s: &'a str) {
            /* nested /* { */ ] */
            let a = r##"{ ( ] unsafe { todo!()"##;
            let b = br#"}"#;
            let c = '\\u{7b}';
            let d = ')';
            let e = "escaped \\" ]";
            // unimplemented!();
        }'''
        self.assertEqual(check.rust_findings(text), [])

    def test_rust_hazards_are_reported_without_including_source(self):
        findings = check.rust_findings("fn test() { unsafe { unimplemented!(); } unbounded_channel(); }")
        codes = {code for code, _, _ in findings}
        self.assertEqual(codes, {"RUST_UNSAFE", "RUST_PLACEHOLDER", "RUST_UNBOUNDED"})
        self.assertTrue(any(code == "RUST_DELIMITER" for code, _, _ in check.rust_findings("fn x() {]")))
        self.assertTrue(any(code == "RUST_LEXICAL" for code, _, _ in check.rust_findings('let x = r#"open')))

    def test_links_support_root_relative_document_relative_and_line_references(self):
        self.write("README.md", "# Overview\n\n## Details\n")
        self.write("src/lib.rs", "// first\n// second\n")
        self.write("docs/example.md", "[root](README.md#details)\n[relative](../README.md)\n[code](src/lib.rs:2)\n")
        checker = self.checker()
        checker.check_docs()
        self.assertEqual(checker.findings, [])

    def test_broken_links_anchors_lines_and_imports_are_reported(self):
        self.write("README.md", "# Intro\n[x](absent.md)\n[x](README.md#missing)\n[x](README.md:999)\n@missing.md\n")
        checker = self.checker()
        checker.check_docs()
        self.assertEqual({item.code for item in checker.findings}, {"DOC_LINK_MISSING", "DOC_ANCHOR", "DOC_LINE"})

    def test_fenced_examples_and_external_urls_are_not_followed(self):
        self.write("README.md", "```text\n[x](absent.md)\n```\n[x](https://invalid.example)\n")
        checker = self.checker()
        checker.check_docs()
        self.assertEqual(checker.findings, [])

    def test_symlink_escape_is_not_read(self):
        outside = self.root.parent / (self.root.name + "-outside.md")
        outside.write_text("private-sentinel", encoding="utf-8")
        self.addCleanup(lambda: outside.unlink(missing_ok=True))
        try:
            (self.root / "link.md").symlink_to(outside)
        except (OSError, NotImplementedError):
            self.skipTest("symlinks are not permitted on this host")
        checker = check.Checker(self.root)
        checker.collect()
        self.assertNotIn("link.md", checker.files)
        self.assertTrue(any(item.code == "PATH_UNSAFE" for item in checker.findings))

    def test_template_values_are_not_exposed_in_diagnostics(self):
        self.write(".env.example", "RECALL_PASSWORD=sensitive-sentinel\nRECALL_BIND=0.0.0.0:6379\nRECALL_WORKERS=100\n")
        checker = self.checker()
        checker.check_env()
        self.assertTrue({"ENV_SECRET", "ENV_BIND", "ENV_RANGE"}.issubset({item.code for item in checker.findings}))
        self.assertNotIn("sensitive-sentinel", str(checker.findings))

    def test_lock_requirement_is_configurable(self):
        self.write("Cargo.toml", '[workspace]\nmembers=[]\n[workspace.lints.rust]\nunsafe_code="forbid"\n')
        checker = self.checker()
        checker.check_text()
        checker.check_manifests()
        self.assertEqual(next(item.severity for item in checker.findings if item.code == "LOCK_MISSING"), "warning")
        checker.require_lock = True
        checker.findings.clear()
        checker.check_manifests()
        self.assertEqual(next(item.severity for item in checker.findings if item.code == "LOCK_MISSING"), "error")

    def test_json_results_and_failure_exit_are_machine_readable(self):
        output = StringIO()
        with redirect_stdout(output):
            result = check.main(["--root", str(self.root), "--format", "json"])
        document = json.loads(output.getvalue())
        self.assertEqual(result, 1)
        self.assertGreater(document["errors"], 0)
        self.assertIn("findings", document)

    def test_missing_private_environment_links_are_allowed_in_clean_checkouts(self):
        self.write("README.md", "[local](.env)\n[private](settings/private.env)\n")
        checker = self.checker()
        checker.check_docs()
        self.assertEqual(checker.findings, [])

    def test_non_regular_input_is_not_opened(self):
        (self.root / "directory.md").mkdir()
        checker = check.Checker(self.root)
        self.assertIsNone(checker.read("directory.md"))
        self.assertEqual(checker.findings[0].code, "FILE_READ")

    def test_malformed_toml_shapes_do_not_crash_manifest_checks(self):
        self.write("Cargo.toml", '[workspace]\nmembers=[]\nlints="invalid"\npackage=[]\ndependencies=42\ntarget="invalid"\n')
        self.write("rust-toolchain.toml", 'toolchain=[]\n')
        checker = self.checker()
        checker.check_text()
        checker.check_manifests()
        checker.check_deployment()
        self.assertTrue(any(item.code == "MANIFEST_SHAPE" for item in checker.findings))

    def test_lock_shape_and_dependency_pins_are_checked(self):
        self.write("Cargo.toml", '[workspace]\nmembers=[]\n[workspace.lints.rust]\nunsafe_code="forbid"\n[workspace.dependencies]\nexample="1.2"\n')
        self.write("Cargo.lock", 'package={name="invalid"}\n')
        checker = self.checker()
        checker.check_text()
        checker.check_manifests()
        codes = {item.code for item in checker.findings}
        self.assertTrue({"LOCK_SHAPE", "DEPENDENCY_PIN"}.issubset(codes))

    def test_deterministic_core_boundary_ignores_comments_but_flags_ambient_calls(self):
        self.write("crates/recall-core/src/lib.rs", '// std::fs is not used\nfn bad() { SystemTime::now(); }\n')
        checker = self.checker()
        checker.check_text()
        self.assertEqual([item.code for item in checker.findings], ["CORE_BOUNDARY"])

    def test_deployment_widening_is_reported(self):
        self.write("compose.yaml", "services:\n  recall:\n    ports:\n      - '6379:6379'\n    privileged: true\n")
        self.write("Dockerfile", "FROM example\nUSER root\nCOPY . /app\n")
        checker = self.checker()
        checker.check_deployment()
        codes = {item.code for item in checker.findings}
        self.assertTrue({"COMPOSE_EXPOSURE", "DOCKER_USER", "DOCKER_COPY"}.issubset(codes))

    def test_lock_bootstrap_needs_no_existing_lock_and_exports_only_the_lock(self):
        docker = (
            "FROM rust:1.85.1-bookworm AS lock-generator\n"
            "WORKDIR /build\nCOPY Cargo.toml rust-toolchain.toml ./\n"
            "COPY crates/ crates/\nRUN cargo generate-lockfile\n"
            "FROM scratch AS lockfile\n"
            "COPY --from=lock-generator /build/Cargo.lock /Cargo.lock\n"
            "FROM rust:1.85.1-bookworm AS build\n"
            "COPY Cargo.lock ./\nRUN cargo build --locked --release -p recall-server\n"
            "FROM debian:bookworm-slim AS runtime\nUSER 10001:10001\n"
            'ENTRYPOINT ["/usr/local/bin/recall-server"]\n'
        )
        self.write("Dockerfile", docker)
        checker = self.checker()
        checker.toml["rust-toolchain.toml"] = {"toolchain": {"channel": "1.85.1"}}
        checker.check_deployment()
        self.assertFalse(any(item.code in {"DOCKER_LOCK_BOOTSTRAP", "DOCKER_LOCK_EXPORT"} for item in checker.findings))

        checker.files["Dockerfile"] = docker.replace("COPY Cargo.toml rust-toolchain.toml ./", "COPY Cargo.toml Cargo.lock rust-toolchain.toml ./")
        checker.findings.clear()
        checker.check_deployment()
        self.assertTrue(any(item.code == "DOCKER_LOCK_BOOTSTRAP" for item in checker.findings))

        checker.files["Dockerfile"] = docker.replace("COPY --from=lock-generator /build/Cargo.lock /Cargo.lock", "COPY --from=lock-generator /build/ /")
        checker.findings.clear()
        checker.check_deployment()
        self.assertTrue(any(item.code == "DOCKER_LOCK_EXPORT" for item in checker.findings))


if __name__ == "__main__":
    unittest.main()
