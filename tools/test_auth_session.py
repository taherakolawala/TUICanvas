import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock
from urllib.request import Request

from tools.auth_session import (CANVAS, NativeCanvas, SessionError, canvas_jar,
                               load_session, profile_id, protect, save_session)
from tools.terminal_login import clean, safe_result


def cookie(domain=CANVAS, name="session", value="SYNTHETIC_SECRET", path="/", expires=-1):
    return {"domain": domain, "name": name, "value": value, "path": path,
            "expires": expires, "httpOnly": True, "secure": True, "sameSite": "Lax"}


class SessionTests(unittest.TestCase):
    def test_cookie_scopes_exclude_identity_provider_and_other_schools(self):
        jar = canvas_jar({"cookies": [cookie(), cookie("login.ufl.edu", "idp"),
                                    cookie("other.instructure.com", "other"),
                                    cookie(".instructure.com", "parent"), cookie(".com", "too_broad")]})
        self.assertEqual({c.name for c in jar}, {"session", "parent"})
        request = Request("https://" + CANVAS + "/api/v1/users/self/profile")
        jar.add_cookie_header(request)
        self.assertIn("session=", request.get_header("Cookie"))
        self.assertNotIn("idp=", request.get_header("Cookie"))

    def test_expired_and_wrong_path_cookies_are_not_sent(self):
        jar = canvas_jar({"cookies": [cookie(name="expired", expires=1), cookie(name="wrong_path", path="/login")]})
        request = Request("https://" + CANVAS + "/api/v1/users/self/profile")
        jar.add_cookie_header(request)
        self.assertIsNone(request.get_header("Cookie"))

    def test_deleted_session_cookie_is_not_resurrected_on_save(self):
        client = NativeCanvas({"cookies": [cookie(), cookie("login.ufl.edu", "idp")]})
        client.jar.clear()
        self.assertEqual([c["name"] for c in client.updated_state()["cookies"]], ["idp"])

    def test_native_reads_cannot_target_another_origin(self):
        client = NativeCanvas({"cookies": []})
        client.opener = Mock()
        for path in ["https://evil.example/api/v1/", "//evil.example/api/v1/", "/courses/1", "/api/v1/\\evil"]:
            with self.subTest(path=path), self.assertRaises(SessionError):
                client.get(path)
        client.opener.open.assert_not_called()

    def test_csrf_header_is_from_canvas_scoped_cookie(self):
        import urllib.error
        client = NativeCanvas({"cookies": [cookie(name="_csrf_token", value="canvas%2Btoken"),
                                           cookie("login.ufl.edu", "_csrf_token", "idp-token")]})
        captured = []
        def record(request, **kwargs):
            captured.append(request)
            raise urllib.error.URLError("synthetic offline test")
        client.opener.open = record
        self.assertEqual(client.get("/api/v1/users/self/profile")["kind"], "network_error")
        self.assertEqual(captured[0].get_header("X-csrf-token"), "canvas+token")

    def test_profile_validation_and_report_do_not_expose_payload(self):
        result = {"kind": "json", "status": 200, "data": {"id": 123, "name": "PRIVATE"}}
        self.assertEqual(profile_id(result), 123)
        self.assertNotIn("PRIVATE", json.dumps(safe_result(result)))
        for value in [True, 0, "123", None]:
            self.assertIsNone(profile_id({"kind": "json", "data": {"id": value}}))

    def test_remote_terminal_escape_sequences_are_removed(self):
        self.assertEqual(clean("\x1b[31mCode\x1b[0m\n123\x07"), "Code\n123")
        self.assertEqual(clean("Safe\x1b]52;c;secret\x07 text"), "Safe text")

    @unittest.skipUnless(os.name == "nt", "Windows DPAPI")
    def test_windows_encryption_round_trip_and_file_has_no_plaintext(self):
        data = b"SYNTHETIC_SECRET"
        encrypted = protect(data)
        self.assertNotIn(data, encrypted)
        self.assertEqual(protect(encrypted, decrypt=True), data)
        directory = Path(__file__).resolve().parents[1] / "benchmarks/private"
        directory.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=directory) as temp:
            path = Path(temp) / "test.session.dpapi"
            save_session({"cookies": [cookie()], "origins": []}, 123, path)
            self.assertNotIn(data, path.read_bytes())
            restored = load_session(path)
            self.assertEqual(restored["user_id"], 123)
            self.assertEqual(restored["browser_state"]["cookies"][0]["value"], data.decode())


if __name__ == "__main__":
    unittest.main()
