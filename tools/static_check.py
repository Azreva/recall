#!/usr/bin/env python3
"""Read-only Recall repository checks. Python 3.11+, standard library only.

This is NOT a Rust parser/compiler, security audit, liveness proof, or Docker
validator. No repository code is imported/executed, and no commands or network
requests are made. Private environment files are deliberately never read.
"""

from __future__ import annotations

import argparse
import ast
from dataclasses import asdict, dataclass
import fnmatch
import ipaddress
import json
import os
from pathlib import Path
import re
import sys
from urllib.parse import unquote

try:
    import tomllib
except ModuleNotFoundError:
    raise SystemExit("Recall static checks require Python 3.11 or newer.") from None


SKIP_DIRS = {".git", "target", "artifacts", "__pycache__", ".venv", "venv", "node_modules"}
TEXT_SUFFIXES = {".rs", ".py", ".toml", ".md", ".yaml", ".yml", ".service"}
TEXT_NAMES = {"Cargo.lock", "Dockerfile", ".gitignore", ".dockerignore", ".env.example"}
REQUIRED = (
    "Cargo.toml", "rust-toolchain.toml", ".cargo/config.toml", "README.md",
    "AGENT.md", "AGENTS.md", "CLAUDE.md", "CONTRIBUTING.md", "ROADMAP.md",
    "docs/commands.md", "docs/validation.md", "docs/deployment.md",
    "plans/architecture.md", "plans/performance.md", ".github/workflows/ci.yml",
    ".gitignore", ".env.example", ".dockerignore", "Dockerfile", "compose.yaml",
    "deploy/recall.service", "tools/static_check.py", "tools/test_static_check.py",
)
MAX_FILE_BYTES = 2 * 1024 * 1024
ENV_NAMES = {
    "RECALL_BIND", "RECALL_WORKERS", "RECALL_IO_THREADS", "RECALL_MAX_MEMORY_MIB",
    "RECALL_KEYS_PER_WORKER", "RECALL_MAX_CONNECTIONS", "RECALL_QUEUE_BYTES_MIB",
    "RECALL_PASSWORD",
}
PIN = re.compile(r"^=\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")


@dataclass(frozen=True)
class Finding:
    severity: str
    code: str
    path: str
    line: int
    message: str


def is_private_env(name: str) -> bool:
    return name != ".env.example" and (
        name == ".env" or name.startswith(".env.") or name.endswith(".env")
    )


def line_at(text: str, index: int) -> int:
    return text.count("\n", 0, index) + 1


def mask_markdown_fences(text: str) -> str:
    """Preserve line numbers while excluding fenced examples from link checks."""
    result = []
    fence = None
    for line in text.splitlines(keepends=True):
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if marker:
            run = marker.group(1)
            if fence is None:
                fence = run
            elif run[0] == fence[0] and len(run) >= len(fence):
                fence = None
            result.append("\n" if line.endswith("\n") else "")
        elif fence:
            result.append("\n" if line.endswith("\n") else "")
        else:
            result.append(line)
    return "".join(result)


def markdown_anchors(text: str) -> set[str]:
    anchors = set()
    counts: dict[str, int] = {}
    for heading in re.findall(r"(?m)^#{1,6}\s+(.+?)\s*#*\s*$", mask_markdown_fences(text)):
        heading = re.sub(r"\[([^]]+)\]\([^)]*\)", r"\1", heading)
        slug = re.sub(r"[^\w -]", "", heading.lower()).replace(" ", "-")
        number = counts.get(slug, 0)
        counts[slug] = number + 1
        anchors.add(slug if number == 0 else f"{slug}-{number}")
    return anchors


def mask_rust(text: str) -> tuple[str, list[tuple[int, str]]]:
    """Lexical masking for delimiter/hazard checks, not Rust grammar analysis."""
    output = list(text)
    errors = []
    index = 0

    def hide(start: int, end: int) -> None:
        for position in range(start, end):
            if output[position] != "\n":
                output[position] = " "

    while index < len(text):
        start = index
        if text.startswith("//", index):
            end = text.find("\n", index)
            index = len(text) if end < 0 else end
            hide(start, index)
        elif text.startswith("/*", index):
            depth = 1
            index += 2
            while index < len(text) and depth:
                if text.startswith("/*", index):
                    depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            if depth:
                errors.append((line_at(text, start), "unterminated block comment"))
            hide(start, index)
        else:
            raw = re.match(r'(?:br|cr|r)(#{0,255})"', text[index:]) if (
                index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_")
            ) else None
            if raw:
                ending = '"' + raw.group(1)
                end = text.find(ending, index + raw.end())
                if end < 0:
                    errors.append((line_at(text, start), "unterminated raw string"))
                    index = len(text)
                else:
                    index = end + len(ending)
                hide(start, index)
            elif text[index] == '"':
                index += 1
                closed = False
                while index < len(text):
                    if text[index] == "\\":
                        index = min(index + 2, len(text))
                    elif text[index] == '"':
                        index += 1
                        closed = True
                        break
                    else:
                        index += 1
                if not closed:
                    errors.append((line_at(text, start), "unterminated string"))
                hide(start, index)
            elif text[index] == "'":
                char = re.match(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\n])|[^'\\\n])'", text[index:])
                if char:
                    index += char.end()
                    hide(start, index)
                else:
                    index += 1  # A lifetime or label is not a character literal.
            else:
                index += 1
    return "".join(output), errors


def rust_findings(text: str) -> list[tuple[str, int, str]]:
    masked, lexical = mask_rust(text)
    found = [("RUST_LEXICAL", line, message) for line, message in lexical]
    stack = []
    pairs = {"(": ")", "[": "]", "{": "}"}
    for index, char in enumerate(masked):
        if char in pairs:
            stack.append((char, index))
        elif char in pairs.values():
            if not stack or pairs[stack[-1][0]] != char:
                found.append(("RUST_DELIMITER", line_at(text, index), "mismatched delimiter"))
                break
            stack.pop()
    else:
        if stack:
            found.append(("RUST_DELIMITER", line_at(text, stack[-1][1]), "unclosed delimiter"))
    for match in re.finditer(r"\b(?:todo|unimplemented)\s*!\s*[({[]", masked):
        found.append(("RUST_PLACEHOLDER", line_at(text, match.start()), "unfinished executable placeholder"))
    for match in re.finditer(r"\bunsafe\s+(?:\{|fn\b|impl\b|trait\b)", masked):
        found.append(("RUST_UNSAFE", line_at(text, match.start()), "unsafe application code violates workspace policy"))
    for match in re.finditer(r"\bunbounded_channel\s*\(", masked):
        found.append(("RUST_UNBOUNDED", line_at(text, match.start()), "unbounded channel requires an explicit resource design"))
    return found


class Checker:
    def __init__(self, root: Path, require_lock: bool = False):
        self.root = root.resolve()
        self.require_lock = require_lock
        self.findings: list[Finding] = []
        self.files: dict[str, str] = {}
        self.toml: dict[str, dict] = {}

    def add(self, severity: str, code: str, path: str, message: str, line: int = 1) -> None:
        finding = Finding(severity, code, path, line, message)
        if finding not in self.findings:
            self.findings.append(finding)

    def contained(self, path: Path) -> bool:
        try:
            path.resolve().relative_to(self.root)
            return True
        except (ValueError, OSError, RuntimeError):
            return False

    def read(self, relative: str) -> str | None:
        if relative in self.files:
            return self.files[relative]
        path = self.root / relative
        if is_private_env(path.name):
            return None
        if not self.contained(path) or path.is_symlink():
            self.add("error", "PATH_UNSAFE", relative, "external/symlink input is not read")
            return None
        try:
            if not path.is_file():
                self.add("error", "FILE_READ", relative, "input must be a regular file")
                return None
            with path.open("rb") as source:
                payload = source.read(MAX_FILE_BYTES + 1)
            if len(payload) > MAX_FILE_BYTES:
                self.add("error", "FILE_SIZE", relative, "static-check input exceeds the 2 MiB limit")
                return None
            text = payload.decode("utf-8-sig").replace("\r\n", "\n")
        except (OSError, UnicodeError):
            self.add("error", "FILE_READ", relative, "cannot read a UTF-8 regular file")
            return None
        self.files[relative] = text
        return text

    def collect(self) -> None:
        if not self.root.is_dir():
            self.add("error", "ROOT_MISSING", ".", "repository root is not a directory")
            return
        for directory, dirs, names in os.walk(self.root, followlinks=False):
            dirs[:] = sorted(name for name in dirs if name not in SKIP_DIRS and not (Path(directory) / name).is_symlink())
            for name in sorted(names):
                if is_private_env(name):
                    continue
                path = Path(directory) / name
                if path.suffix in TEXT_SUFFIXES or name in TEXT_NAMES:
                    self.read(path.relative_to(self.root).as_posix())
        for relative in REQUIRED:
            if relative not in self.files:
                self.add("error", "FILE_REQUIRED", relative, "required repository file is missing or unreadable")

    def check_text(self) -> None:
        for relative, text in list(self.files.items()):
            for match in re.finditer(r"(?m)^(?:<{7} .+|={7}|>{7} .+)$", text):
                self.add("error", "MERGE_MARKER", relative, "unresolved merge marker", line_at(text, match.start()))
            if "\0" in text:
                self.add("error", "TEXT_NUL", relative, "unexpected NUL byte in source/configuration")
            if relative.endswith(".py"):
                try:
                    ast.parse(text, filename=relative, feature_version=(3, 11))
                except SyntaxError as error:
                    self.add("error", "PYTHON_SYNTAX", relative, "invalid Python 3.11-compatible syntax", error.lineno or 1)
            elif relative.endswith(".rs"):
                for code, line, message in rust_findings(text):
                    self.add("error", code, relative, message, line)
                if relative.startswith("crates/recall-core/src/"):
                    masked, _ = mask_rust(text)
                    for match in re.finditer(r"\bstd::(?:fs|net|thread)\b|\bSystemTime\s*::\s*now\b|\btokio\s*::", masked):
                        self.add("error", "CORE_BOUNDARY", relative, "deterministic core must not depend on ambient I/O, threads, or wall time", line_at(text, match.start()))
            if relative.endswith(".toml") or relative == "Cargo.lock":
                try:
                    self.toml[relative] = tomllib.loads(text)
                except tomllib.TOMLDecodeError:
                    self.add("error", "TOML_SYNTAX", relative, "invalid TOML")

    def resolve_link(self, source: str, target: str) -> Path | None:
        # Existing editor links are repository-root-relative. Explicit ./ and
        # ../ paths are document-relative; normal Markdown relatives also work.
        if target.startswith("/"):
            candidates = [self.root / target.lstrip("/")]
        elif target.startswith(("./", "../")):
            candidates = [(self.root / source).parent / target]
        else:
            candidates = [self.root / target, (self.root / source).parent / target]
        for candidate in candidates:
            if self.contained(candidate) and candidate.exists():
                return candidate.resolve()
        return None

    def check_link(self, source: str, target: str, line: int) -> None:
        target = target.strip().strip("<>")
        if re.match(r"^(?:https?|mailto):", target, re.I):
            return  # Offline means external links are neither contacted nor certified.
        path, _, anchor = target.partition("#")
        path = unquote(path)
        if is_private_env(Path(path).name):
            return  # Private local configuration need not exist in a checkout.
        number = None
        editor_line = re.fullmatch(r"(.+):(\d+)", path)
        if editor_line:
            path, number = editor_line.group(1), int(editor_line.group(2))
        elif re.match(r"^[A-Za-z][A-Za-z0-9+.-]*:", path):
            self.add("error", "DOC_LINK_SCHEME", source, "unsupported local-link scheme", line)
            return
        if not path:
            resolved = self.root / source
        else:
            resolved = self.resolve_link(source, path)
        if resolved is None:
            if path == "Cargo.lock" or path == "../Cargo.lock":
                return  # The single lock diagnostic handles generated lock absence.
            self.add("error", "DOC_LINK_MISSING", source, f"local target does not exist: {path}", line)
            return
        if is_private_env(resolved.name):
            return  # Links to local configuration do not authorize reading secrets.
        relative = resolved.relative_to(self.root).as_posix()
        if number is not None:
            text = self.files.get(relative)
            if number < 1 or (text is not None and number > max(1, len(text.splitlines()))):
                self.add("error", "DOC_LINE", source, "source-line reference is out of range", line)
        if anchor and resolved.suffix == ".md":
            text = self.files.get(relative, "")
            if unquote(anchor) not in markdown_anchors(text):
                self.add("error", "DOC_ANCHOR", source, f"heading anchor does not exist: {anchor}", line)

    def check_docs(self) -> None:
        for source, original in list(self.files.items()):
            if not source.endswith(".md"):
                continue
            text = mask_markdown_fences(original)
            for match in re.finditer(r"\[[^\]\n]*\]\(([^)\n]+)\)", text):
                self.check_link(source, match.group(1), line_at(text, match.start()))
            for match in re.finditer(r"(?m)^@([\w./-]+)\s*$", text):
                self.check_link(source, match.group(1), line_at(text, match.start()))

    def table(self, value: object, path: str, section: str) -> dict:
        if isinstance(value, dict):
            return value
        self.add("error", "MANIFEST_SHAPE", path, f"{section} must be a table")
        return {}

    def check_manifests(self) -> None:
        root = self.toml.get("Cargo.toml", {})
        workspace = self.table(root.get("workspace", {}), "Cargo.toml", "workspace")
        if not isinstance(workspace.get("members"), list):
            self.add("error", "WORKSPACE", "Cargo.toml", "workspace members must be declared")
            return
        lints = self.table(workspace.get("lints", {}), "Cargo.toml", "workspace.lints")
        rust_lints = self.table(lints.get("rust", {}), "Cargo.toml", "workspace.lints.rust")
        if rust_lints.get("unsafe_code") != "forbid":
            self.add("error", "UNSAFE_POLICY", "Cargo.toml", "workspace must forbid unsafe application code")
        shared_dependencies = self.table(workspace.get("dependencies", {}), "Cargo.toml", "workspace.dependencies")
        for member in workspace["members"]:
            if not isinstance(member, str) or any(char in member for char in "*?["):
                self.add("error", "WORKSPACE_MEMBER", "Cargo.toml", "checker requires explicit local workspace member paths")
                continue
            if Path(member).is_absolute() or ".." in Path(member).parts:
                self.add("error", "WORKSPACE_MEMBER", "Cargo.toml", "workspace members must stay inside the repository")
                continue
            manifest = f"{member}/Cargo.toml"
            data = self.toml.get(manifest)
            if data is None:
                self.add("error", "WORKSPACE_MEMBER", manifest, "workspace member manifest is missing or invalid")
                continue
            if self.table(data.get("lints", {}), manifest, "lints").get("workspace") is not True:
                self.add("error", "MEMBER_LINTS", manifest, "member must inherit workspace lints")
            if not any((self.root / member / "src" / name).is_file() for name in ("lib.rs", "main.rs")) and not data.get("lib") and not data.get("bin"):
                self.add("error", "MEMBER_TARGET", manifest, "member has no declared/default Rust target")
        pinned = []
        for relative, manifest in self.toml.items():
            if not relative.endswith("Cargo.toml"):
                continue
            tables = [manifest, self.table(manifest.get("workspace", {}), relative, "workspace")]
            targets = self.table(manifest.get("target", {}), relative, "target")
            tables.extend(self.table(target, relative, "target entry") for target in targets.values())
            for table in tables:
                if not isinstance(table, dict):
                    continue
                for section in ("dependencies", "dev-dependencies", "build-dependencies"):
                    dependencies = table.get(section, {})
                    if not isinstance(dependencies, dict):
                        self.add("error", "DEPENDENCY_TABLE", relative, "dependency section must be a table")
                        continue
                    for name, dependency in dependencies.items():
                        detail = {"version": dependency} if isinstance(dependency, str) else dependency
                        if not isinstance(detail, dict):
                            self.add("error", "DEPENDENCY_VALUE", relative, "invalid dependency declaration")
                            continue
                        if detail.get("workspace") is True:
                            if name not in shared_dependencies:
                                self.add("error", "DEPENDENCY_INHERIT", relative, "dependency is not defined in the workspace")
                        elif "path" in detail:
                            target = (self.root / relative).parent / str(detail["path"]) / "Cargo.toml"
                            if not self.contained(target) or not target.is_file():
                                self.add("error", "DEPENDENCY_PATH", relative, "local dependency manifest is missing or outside the repository")
                        elif "git" in detail:
                            self.add("warning", "DEPENDENCY_GIT", relative, "review remote dependency provenance and immutable revision")
                        elif not isinstance(detail.get("version"), str) or not PIN.fullmatch(detail["version"]):
                            self.add("error", "DEPENDENCY_PIN", relative, "direct registry dependencies must use exact version pins")
                        else:
                            pinned.append((detail.get("package", name), detail["version"][1:]))
        toolchain = self.table(self.toml.get("rust-toolchain.toml", {}).get("toolchain", {}), "rust-toolchain.toml", "toolchain")
        channel = toolchain.get("channel", "")
        if not isinstance(channel, str) or not re.fullmatch(r"\d+\.\d+\.\d+", channel):
            self.add("error", "TOOLCHAIN_PIN", "rust-toolchain.toml", "toolchain must pin a release version")
        else:
            minimum = self.table(workspace.get("package", {}), "Cargo.toml", "workspace.package").get("rust-version", "")
            if not isinstance(minimum, str) or not re.fullmatch(r"\d+\.\d+(?:\.\d+)?", minimum):
                self.add("error", "TOOLCHAIN_MINIMUM", "Cargo.toml", "declare the minimum Rust version")
            elif tuple(map(int, channel.split("."))) < tuple(map(int, minimum.split("."))) + ((0,) if minimum.count(".") == 1 else ()):
                self.add("error", "TOOLCHAIN_MINIMUM", "rust-toolchain.toml", "pinned toolchain is older than the declared minimum")
        lock = self.toml.get("Cargo.lock")
        if lock is None:
            self.add("error" if self.require_lock else "warning", "LOCK_MISSING", "Cargo.lock", "generate and review the real lock on a Rust-enabled machine before locked or Docker builds")
        else:
            packages = lock.get("package", [])
            if not isinstance(packages, list) or not all(
                isinstance(package, dict) and isinstance(package.get("name"), str) and isinstance(package.get("version"), str)
                for package in packages
            ):
                self.add("error", "LOCK_SHAPE", "Cargo.lock", "lock packages must contain string names and versions")
                packages = []
            versions = {(package["name"], package["version"]) for package in packages}
            for package in pinned:
                if package not in versions:
                    self.add("error", "LOCK_DIRECT_PIN", "Cargo.lock", "lock does not contain a pinned direct dependency; regenerate with Cargo")

    def check_env(self) -> None:
        text = self.files.get(".env.example", "")
        values = {}
        for line, raw in enumerate(text.splitlines(), 1):
            value = raw.strip()
            if not value or value.startswith("#"):
                continue
            key, equal, value = value.partition("=")
            key, value = key.strip(), value.strip()
            if not equal or key not in ENV_NAMES or len(raw.encode("utf-8")) > 2048:
                self.add("error", "ENV_SYNTAX", ".env.example", "invalid or unknown template assignment", line)
                continue
            if key in values:
                self.add("error", "ENV_DUPLICATE", ".env.example", "duplicate template assignment", line)
            if value.startswith(("'", '"')):
                if len(value) < 2 or value[-1] != value[0]:
                    self.add("error", "ENV_QUOTE", ".env.example", "unclosed quoted value", line)
                    continue
                value = value[1:-1]
            values[key] = value
            if key == "RECALL_PASSWORD":
                self.add("error", "ENV_SECRET", ".env.example", "do not provide an active password/default credential in the tracked template", line)
            elif key == "RECALL_BIND":
                host, separator, port = value.rpartition(":")
                try:
                    if not separator or not ipaddress.ip_address(host.strip("[]")).is_loopback or not 1 <= int(port) <= 65535:
                        raise ValueError
                except ValueError:
                    self.add("error", "ENV_BIND", ".env.example", "deployment template must use a loopback address and valid nonzero port", line)
            elif not re.fullmatch(r"[0-9]+", value):
                self.add("error", "ENV_NUMBER", ".env.example", "numeric setting must be a decimal integer", line)
            else:
                maximum = {
                    "RECALL_WORKERS": 64, "RECALL_IO_THREADS": 64,
                    "RECALL_KEYS_PER_WORKER": 1_048_576, "RECALL_MAX_CONNECTIONS": 65_536,
                    "RECALL_QUEUE_BYTES_MIB": 4095,
                }.get(key, ((1 << 64) - 1) // (1024 * 1024))
                if not 1 <= int(value) <= maximum:
                    self.add("error", "ENV_RANGE", ".env.example", "template setting is outside the supported range", line)
        loader = self.files.get("crates/recall-server/src/settings.rs", "")
        schema = re.search(r"const ENV_NAMES:.*?=\s*&\[(.*?)\];", loader, re.S)
        names = set(re.findall(r'"(RECALL_[A-Z_]+)"', schema.group(1))) if schema else set()
        if names != ENV_NAMES:
            self.add("error", "ENV_SCHEMA", "crates/recall-server/src/settings.rs", "keep the checker and loader setting-name schemas in sync")
        ignore = self.files.get(".gitignore", "")
        for name, expected in ((".env", True), (".env.local", True), (".env.example", False)):
            ignored = False
            for pattern in ignore.splitlines():
                pattern = pattern.strip()
                if not pattern or pattern.startswith("#"):
                    continue
                negative = pattern.startswith("!")
                pattern = pattern.removeprefix("!").removeprefix("/")
                if fnmatch.fnmatchcase(name, pattern):
                    ignored = not negative
            if ignored != expected:
                self.add("error", "ENV_IGNORE", ".gitignore", "ignore private environment files but allow the tracked example")

    def check_deployment(self) -> None:
        docker = self.files.get("Dockerfile", "")
        toolchain = self.table(self.toml.get("rust-toolchain.toml", {}).get("toolchain", {}), "rust-toolchain.toml", "toolchain")
        channel = toolchain.get("channel", "")
        if f"FROM rust:{channel}-bookworm AS build" not in docker:
            self.add("error", "DOCKER_TOOLCHAIN", "Dockerfile", "build image must match the pinned Rust toolchain")
        if "cargo build --locked --release -p recall-server" not in docker:
            self.add("error", "DOCKER_LOCK", "Dockerfile", "Docker builds must consume the reviewed lock")
        generator = re.search(
            rf"(?ms)^FROM rust:{re.escape(str(channel))}-bookworm AS lock-generator\s*\n(.*?)(?=^FROM |\Z)", docker
        )
        if generator is None or "RUN cargo generate-lockfile" not in generator.group(1):
            self.add("error", "DOCKER_LOCK_BOOTSTRAP", "Dockerfile", "provide the explicit pinned-toolchain lock bootstrap target")
        elif re.search(r"(?m)^COPY[^\n]*\bCargo\.lock\b", generator.group(1)):
            self.add("error", "DOCKER_LOCK_BOOTSTRAP", "Dockerfile", "lock bootstrap must not require an existing lock input")
        export = re.search(r"(?ms)^FROM scratch AS lockfile\s*\n(.*?)(?=^FROM |\Z)", docker)
        if export is None or export.group(1).strip() != "COPY --from=lock-generator /build/Cargo.lock /Cargo.lock":
            self.add("error", "DOCKER_LOCK_EXPORT", "Dockerfile", "lock export target must contain only the generated dependency lock")
        runtime = re.split(r"(?m)^FROM ", docker)[-1]
        if not re.search(r"(?m)^USER (?!0(?::|$)|root(?::|$))\S+", runtime):
            self.add("error", "DOCKER_USER", "Dockerfile", "runtime image must declare a non-root user")
        if not re.search(r'(?m)^ENTRYPOINT \["/usr/local/bin/recall-server"\]$', runtime):
            self.add("error", "DOCKER_ENTRYPOINT", "Dockerfile", "use exec-form server entrypoint for signal delivery")
        if re.search(r"(?im)^\s*(?:COPY|ADD)\s+(?:\.\s|\.env\b)", docker):
            self.add("error", "DOCKER_COPY", "Dockerfile", "do not copy the whole repository or environment files into the build")
        ignore = self.files.get(".dockerignore", "").splitlines()
        for pattern in ("**", "**/.env", "**/.env.*", "**/*.env"):
            if pattern not in ignore:
                self.add("error", "DOCKER_SECRETS", ".dockerignore", "retain allowlisted build inputs and explicit environment-file exclusions")
        compose = self.files.get("compose.yaml", "")
        policy = {
            "COMPOSE_NETWORK": r"(?m)^\s+network_mode:\s*host\s*$",
            "COMPOSE_READONLY": r"(?m)^\s+read_only:\s*true\s*$",
            "COMPOSE_USER": r'(?m)^\s+user:\s*"10001:10001"\s*$',
            "COMPOSE_MOUNT": r"(?m)^\s+create_host_path:\s*false\s*$",
            "COMPOSE_PRIVILEGES": r"(?m)^\s+- no-new-privileges:true\s*$",
        }
        for code, pattern in policy.items():
            if not re.search(pattern, compose):
                self.add("error", code, "compose.yaml", "local-container deployment safety policy is missing")
        if re.search(r"(?m)^\s+ports:|^\s+privileged:\s*true", compose) or "/var/run/docker.sock" in compose:
            self.add("error", "COMPOSE_EXPOSURE", "compose.yaml", "do not publish ports, run privileged, or mount the Docker socket")
        service = self.files.get("deploy/recall.service", "")
        for setting in ("User=recall", "NoNewPrivileges=true", "ProtectSystem=strict", "LimitCORE=0"):
            if setting not in service.splitlines():
                self.add("error", "SERVICE_POLICY", "deploy/recall.service", "required service hardening setting is missing")

    def run(self) -> list[Finding]:
        self.collect()
        self.check_text()
        self.check_manifests()
        self.check_docs()
        self.check_env()
        self.check_deployment()
        self.findings.sort(key=lambda item: (item.path, item.line, item.code, item.severity))
        return self.findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent, help="repository root (default: script's repository)")
    parser.add_argument("--format", choices=("text", "json"), default="text")
    parser.add_argument("--require-lock", action="store_true", help="treat absent Cargo.lock as an error for release/deployment preparation")
    parser.add_argument("--strict", action="store_true", help="fail on warnings as well as errors")
    arguments = parser.parse_args(argv)
    checker = Checker(arguments.root, require_lock=arguments.require_lock)
    findings = checker.run()
    errors = sum(item.severity == "error" for item in findings)
    warnings = sum(item.severity == "warning" for item in findings)
    if arguments.format == "json":
        print(json.dumps({
            "scope": "read-only repository checks; no Rust compilation, tests, network, or Docker validation",
            "files_checked": len(checker.files), "errors": errors, "warnings": warnings,
            "findings": [asdict(item) for item in findings],
        }, indent=2))
    else:
        for finding in findings:
            print(f"{finding.severity.upper()} {finding.code} {finding.path}:{finding.line}: {finding.message}")
        print(f"Static checks: {len(checker.files)} files, {errors} errors, {warnings} warnings.")
        print("Rust compilation, runtime correctness, Docker/YAML validation, and external links were NOT checked.")
    return 1 if errors or (arguments.strict and warnings) else 0


if __name__ == "__main__":
    raise SystemExit(main())
