import json
import unittest
from unittest.mock import Mock

from tools.auth_probe import Page, Probe, ScopedRedirects, UnsupportedFlow, trusted_url
from urllib.request import Request


class AuthProbeTests(unittest.TestCase):
    def test_login_submits_clicked_button_action(self):
        page = Page("https://login.ufl.edu/idp?execution=test", '<form action="?execution=test" method="post"><input name="j_username"><input name="j_password" type="password"><button type="submit" name="_eventId_proceed" value="continue">LOGIN</button></form>')
        sent = []
        probe = Probe()
        probe.fetch = lambda url, fields: sent.append(dict(fields))
        probe.login(page, "fixture-user", "fixture-password")
        self.assertEqual(sent[0].get("_eventId_proceed"), "continue")

    def test_missing_or_disabled_submit_action_does_not_send_credentials(self):
        for control in ['', '<button type="submit" name="_eventId_proceed" disabled>LOGIN</button>', '<button type="button" name="_eventId_proceed">LOGIN</button>']:
            with self.subTest(control=control):
                page = Page("https://login.ufl.edu/idp", '<form method="post"><input name="j_username"><input name="j_password">' + control + '</form>')
                probe = Probe()
                probe.fetch = Mock()
                with self.assertRaises(UnsupportedFlow):
                    probe.login(page, "fixture-user", "fixture-password")
                probe.fetch.assert_not_called()

    def test_only_selected_submitter_is_sent_for_input_and_default_button(self):
        for control in ['<input type="submit" name="_eventId_proceed" value="go">', '<button name="_eventId_proceed" value="go">LOGIN</button>']:
            with self.subTest(control=control):
                page = Page("https://login.ufl.edu/idp", '<form method="post"><input name="j_username"><input name="j_password">' + control + '<button name="cancel">Cancel</button></form>')
                sent = []
                probe = Probe()
                probe.fetch = lambda url, fields: sent.append(dict(fields))
                probe.login(page, "fixture-user", "fixture-password")
                self.assertEqual(sent[0]["_eventId_proceed"], "go")
                self.assertNotIn("cancel", sent[0])

    def test_origin_validation(self):
        for url in ["http://login.ufl.edu/", "https://login.ufl.edu.evil.test/", "https://evil.test/", "https://a.duosecurity.com.evil.test/", "https://user:secret@login.ufl.edu/", "https://login.ufl.edu:8443/"]:
            self.assertFalse(trusted_url(url), url)
        self.assertTrue(trusted_url("https://login.ufl.edu/idp"))
        self.assertTrue(trusted_url("https://api-example.duosecurity.com/"))

    def test_report_excludes_secret_values_and_url_parameters(self):
        page = Page("https://login.ufl.edu/idp?execution=SECRET_STATE", '<form action="?token=SECRET_TOKEN" method="post"><input name="j_username" value="SECRET_USER"><input type="password" name="j_password" value="SECRET_PASSWORD"><input type="hidden" name="csrf" value="SECRET_CSRF"></form>')
        report = page.report()
        self.assertEqual(report["stage"], "username_password_form")
        self.assertNotIn("SECRET", json.dumps(report))

    def test_credentials_never_post_to_a_non_uf_form(self):
        page = Page("https://login.ufl.edu/idp", '<form action="https://api-example.duosecurity.com/" method="post"><input name="j_username"><input name="j_password"></form>')
        probe = Probe()
        probe.fetch = Mock()
        with self.assertRaises(UnsupportedFlow):
            probe.login(page, "user", "password")
        probe.fetch.assert_not_called()

    def test_preserved_credential_redirect_is_rejected(self):
        req = Request("https://login.ufl.edu/idp", data=b"password=secret")
        with self.assertRaises(UnsupportedFlow):
            ScopedRedirects().redirect_request(req, None, 307, "", {}, "https://api-example.duosecurity.com/")

    def test_unchecked_and_disabled_fields_are_not_submitted(self):
        page = Page("https://login.ufl.edu/", '<form><input name="hidden" type="hidden" value="yes"><input name="unchecked" type="checkbox"><input name="disabled" disabled><input name="submit" type="submit" value="go"></form>')
        self.assertEqual(page.forms[0].fields(), {"hidden": "yes"})


if __name__ == "__main__":
    unittest.main()
