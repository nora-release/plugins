#!/usr/bin/env python3
"""Exercise Git's real credential prompt without network access or real secrets."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parent / "cargo-git-auth.sh"
TOKEN = "nora-ci-test-only-token"


class CargoGitAuthTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="nora-cargo-auth-test-")
        self.directory = Path(self.temporary.name)
        self.environment = {
            "PATH": os.environ["PATH"],
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "NORA_SOURCE_TOKEN": TOKEN,
        }

    def tearDown(self):
        self.temporary.cleanup()

    def run_script(self, *arguments, stdin=None, environment=None, script=SCRIPT):
        return subprocess.run(
            ["bash", str(script), *arguments],
            input=stdin,
            text=True,
            capture_output=True,
            cwd=self.directory,
            env=self.environment if environment is None else environment,
            check=False,
        )

    def fill(self, url, **kwargs):
        return self.run_script("--", "git", "credential", "fill", stdin=f"url={url}\n\n", **kwargs)

    def test_git_receives_only_the_requested_repository_credential(self):
        for suffix in ["nora", "nora.git"]:
            with self.subTest(suffix=suffix):
                result = self.fill(f"https://github.com/nora-release/{suffix}")
                self.assertEqual(result.returncode, 0, result.stderr)
                credential = dict(line.split("=", 1) for line in result.stdout.splitlines())
                self.assertEqual(credential["username"], "x-access-token")
                self.assertEqual(credential["password"], TOKEN)
                self.assertEqual(credential["path"], f"nora-release/{suffix}")
                self.assertNotIn(TOKEN, result.stderr)

    def test_other_hosts_repositories_and_url_forms_never_receive_the_token(self):
        for url in [
            "https://github.com/nora-release/plugin-kit.git",
            "https://github.com/nora-release/nora-other.git",
            "https://github.com/nora-release/nora.git/extra",
            "https://github.com/nora-release/nora.git?redirect=elsewhere",
            "https://github.com/other/nora.git",
            "https://github.com.evil.invalid/nora-release/nora.git",
            "https://evil.invalid/nora-release/nora.git",
            "http://github.com/nora-release/nora.git",
            "https://github.com:443/nora-release/nora.git",
            "https://someone@github.com/nora-release/nora.git",
        ]:
            with self.subTest(url=url):
                result = self.fill(url)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn(TOKEN, result.stdout + result.stderr)

    def test_askpass_returns_a_username_without_exposing_the_password(self):
        result = self.run_script("Username for 'https://github.com/nora-release/nora.git': ")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "x-access-token\n")

    def test_unrecognized_prompt_is_refused_silently(self):
        for prompt in [
            "Password for 'https://github.com': ",
            "Password for 'https://x-access-token@github.com/nora-release/plugin-kit.git': ",
            "Password for 'https://x-access-token@github.com/nora-release/nora.git': \nextra",
            "Enter passphrase:",
        ]:
            with self.subTest(prompt=prompt):
                result = self.run_script(prompt)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertNotIn(TOKEN, result.stderr)

    def test_missing_or_multiline_token_stops_before_the_command_runs(self):
        marker = self.directory / "command-ran"
        for value in [None, "", "one\ntwo", "one\rtwo"]:
            with self.subTest(value=value):
                environment = self.environment.copy()
                if value is None:
                    environment.pop("NORA_SOURCE_TOKEN")
                else:
                    environment["NORA_SOURCE_TOKEN"] = value
                result = self.run_script("--", "touch", str(marker), environment=environment)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(marker.exists())
                self.assertNotIn(TOKEN, result.stdout + result.stderr)

    def test_the_token_is_absent_from_arguments_git_configuration_and_disk(self):
        code = "import json,os,sys; print(json.dumps({'argv':sys.argv,'git':{k:v for k,v in os.environ.items() if k.startswith('GIT_CONFIG_')},'cli':os.environ.get('CARGO_NET_GIT_FETCH_WITH_CLI'),'prompt':os.environ.get('GIT_TERMINAL_PROMPT')}))"
        before = set(self.directory.iterdir())
        result = self.run_script("--", sys.executable, "-c", code)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(TOKEN, result.stdout + result.stderr)
        values = json.loads(result.stdout)
        self.assertEqual(values["cli"], "true")
        self.assertEqual(values["prompt"], "0")
        self.assertEqual(before, set(self.directory.iterdir()))
        result = self.run_script("--", "git", "config", "--list")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(TOKEN, result.stdout + result.stderr)
        self.assertIn("credential.usehttppath=true", result.stdout)
        self.assertIn("http.followredirects=false", result.stdout)

    def test_inherited_credential_helpers_are_not_used_or_allowed_to_store_the_token(self):
        marker = self.directory / "helper-ran"
        environment = self.environment | {
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "credential.helper",
            "GIT_CONFIG_VALUE_0": f"!touch {shlex.quote(str(marker))}",
        }
        result = self.fill("https://github.com/nora-release/nora.git", environment=environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"password={TOKEN}\n", result.stdout)
        self.assertFalse(marker.exists())

    def test_unrelated_process_git_configuration_is_preserved(self):
        environment = self.environment | {
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "core.noraTest",
            "GIT_CONFIG_VALUE_0": "preserved",
        }
        result = self.run_script("--", "git", "config", "--get", "core.noraTest", environment=environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "preserved\n")

    def test_git_tracing_is_disabled_for_the_authenticated_process(self):
        trace = self.directory / "trace"
        environment = self.environment | {
            "GIT_TRACE": str(trace),
            "GIT_TRACE_CURL": "1",
            "GIT_TRACE2_ENV_VARS": "NORA_SOURCE_TOKEN",
            "GIT_CURL_VERBOSE": "1",
        }
        result = self.fill("https://github.com/nora-release/nora.git", environment=environment)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(TOKEN, result.stderr)
        self.assertFalse(trace.exists())

    def test_askpass_works_from_a_path_containing_spaces(self):
        script = self.directory / "path with spaces" / "cargo-git-auth.sh"
        script.parent.mkdir()
        shutil.copy2(SCRIPT, script)
        result = self.fill("https://github.com/nora-release/nora.git", script=script)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"password={TOKEN}\n", result.stdout)
        self.assertNotIn(TOKEN, script.read_text())

    def test_command_exit_status_is_preserved(self):
        result = self.run_script("--", "bash", "-c", "exit 37")
        self.assertEqual(result.returncode, 37)
        self.assertEqual(result.stdout + result.stderr, "")

    def test_invalid_existing_git_configuration_fails_closed(self):
        environment = self.environment | {"GIT_CONFIG_COUNT": "not-a-count"}
        result = self.run_script("--", "true", environment=environment)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(TOKEN, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
