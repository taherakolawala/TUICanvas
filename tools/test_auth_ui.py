import io
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from tools.auth_ui import Status, duo_view, transition_view
from tools.terminal_login import browser_login


def controls(*labels):
    return [(None, {"label": text, "type": "button"}, "https://api-example.duosecurity.com/") for text in labels]


VERIFIED = '''Enter code in Duo Mobile
Verify it's you by entering this verification code in the Duo Mobile app…
123
Sent to "Test phone" (•••-•••-0123)
Sent to phone number ending in 0123
Other options
Need help?Secured by Duo
Now on page called, Loading'''


class LoginUITests(unittest.TestCase):
    def test_verified_push_extracts_code_without_loading_announcement_or_device_duplicate(self):
        view = duo_view(VERIFIED, controls("Other options", "Need help?"))
        self.assertEqual(view.kind, "verified_push")
        self.assertTrue(view.automatic)
        self.assertEqual(view.title, "Enter 123 in Duo Mobile to approve")
        self.assertEqual(view.lines, ())
        self.assertEqual(view.actions, (("o", 0, "other options"),))

    def test_remember_device_requires_explicit_choice_with_correct_mapping(self):
        view = duo_view("Is this your device?", controls("No, other people use this device", "Yes, this is my device"))
        self.assertEqual(view.kind, "remember")
        self.assertEqual(view.actions[0][:2], ("y", 1))
        self.assertEqual(view.actions[1][:2], ("n", 0))

    def test_loading_and_success_do_not_require_enter(self):
        for text in ["Secured by Duo\nNow on page called, Loading", "Success!\nLogging you in…\nSecured by Duo"]:
            self.assertTrue(duo_view(text, []).automatic)

    def test_unfamiliar_challenge_keeps_instructions_and_real_options(self):
        view = duo_view("Enter a passcode from your security key", controls("Passcode", "Verify", "Need help?"))
        self.assertFalse(view.automatic)
        self.assertIn("Enter a passcode from your security key", view.lines)
        self.assertEqual([a[2] for a in view.actions], ["Passcode", "Verify"])

    def test_missing_code_is_not_invented_from_phone_suffix(self):
        view = duo_view("Enter code in Duo Mobile\nSent to phone number ending in 0123", controls("Other options"))
        self.assertEqual(view.kind, "code_loading")
        self.assertNotIn("0123", view.title + " ".join(view.lines))

    def test_plain_duo_push_waits_automatically(self):
        # Observed 2026-10-20: a push without a code first, then Verified Push.
        text = """Check for a Duo Push
Verify it's you by approving the notification…
Sent to "iOS" (•••-•••-0581)
Sent to phone number ending in 0581
Remember me"""
        view = duo_view(text, controls("Other options", "Remember me", "Need help?"))
        self.assertEqual(view.kind, "push")
        self.assertTrue(view.automatic)
        self.assertEqual(view.title, "Approve the Duo Push on your phone (iOS)")
        self.assertNotIn("0581", view.title)

    def test_status_line_updates_in_place_on_a_terminal(self):
        class Terminal(io.StringIO):
            encoding = "utf-8"
            def isatty(self):
                return True
        out = Terminal()
        status = Status(out)
        status.show("Waiting for Duo", "q cancel")
        status.show("Waiting for Duo", "q cancel")
        status.show("Enter 123 in Duo Mobile to approve")
        status.done("Signed in.")
        status.close()
        text = out.getvalue()
        self.assertIn("\r⠋ Waiting for Duo…   (q cancel)", text)
        self.assertIn("\r⠋ Enter 123 in Duo Mobile to approve…", text)
        self.assertTrue(text.endswith("✓ Signed in.\n"))
        self.assertEqual(text.count("\n"), 1, "progress never scrolls the terminal")

    def test_status_prints_each_state_once_when_not_a_terminal(self):
        out = io.StringIO()
        status = Status(out)
        for _ in range(3):
            status.show("Waiting for Duo")
        status.done("Signed in.")
        self.assertEqual(out.getvalue(), "Waiting for Duo...\nOK Signed in.\n")

    def test_normal_duo_flow_completes_without_manual_refresh(self):
        # Replay the observed sequence without credentials, real browser, or MFA.
        screens = [("Secured by Duo\nNow on page called, Loading", []),
                   (VERIFIED, controls("Other options")),
                   ("Is this your device?", controls("Yes, this is my device", "No, other people use this device")),
                   ("Success!\nLogging you in…", [])]
        page = Mock()
        page.url = "https://api-example.duosecurity.com/"
        phase = [-1]
        def tick(_):
            phase[0] += 1
            if phase[0] == len(screens):
                page.url = "https://ufl.instructure.com/"
        page.wait_for_timeout.side_effect = tick
        def locator(selector):
            result = Mock()
            result.count.return_value = 1 if selector == "#global_nav_profile_link" and phase[0] == len(screens) else 0
            return result
        page.locator.side_effect = locator
        runtime = Mock()
        context = runtime.chromium.launch.return_value.new_context.return_value
        context.new_page.return_value = page
        context.storage_state.return_value = {"cookies": [], "origins": []}
        manager = Mock()
        manager.__enter__ = Mock(return_value=runtime)
        manager.__exit__ = Mock(return_value=False)
        args = SimpleNamespace(channel="msedge", browser=False, show_password=False)
        with patch("playwright.sync_api.sync_playwright", return_value=manager), \
             patch("tools.terminal_login.describe_controls", side_effect=lambda _: screens[phase[0]]), \
             patch("tools.terminal_login.poll_key", side_effect=lambda keys=iter([None, None, "y"]): next(keys, None)), \
             patch("tools.terminal_login.interact") as interact, \
             patch("tools.terminal_login.verify_and_save", return_value=(True, {"kind": "json", "status": 200})), \
             patch("tools.terminal_login.write_report"), \
             patch("builtins.input", side_effect=AssertionError("Unexpected blocking prompt")), \
             patch("sys.stdout", new_callable=io.StringIO):
            self.assertEqual(browser_login(args, None), 0)
            interact.assert_called_once()
            self.assertEqual(interact.call_args.args[3], "1")
        context.close.assert_called_once()

    def test_uf_saving_page_is_one_line_until_it_is_stuck(self):
        text = "UF Authentication - GatorLink Login\nUNIVERSITY OF FLORIDA LOGIN SERVICE - SAVING SESSION INFORMATION...\nSaving login session information to the browser..."
        view = transition_view("login.ufl.edu", text, [], 2.0)
        self.assertEqual(view.kind, "uf_saving")
        self.assertEqual(view.lines, ())
        self.assertIsNone(transition_view("login.ufl.edu", text, [], 61.0))

    def test_other_uf_pages_show_details_once_they_linger(self):
        self.assertEqual(transition_view("login.ufl.edu", "Please wait", [], 1.0).kind, "uf_waiting")
        self.assertIsNone(transition_view("login.ufl.edu", "Your password has expired", [], 7.0))
        self.assertIsNone(transition_view("login.ufl.edu", "x", controls("Continue"), 1.0))
        self.assertIsNone(transition_view("api-example.duosecurity.com", "x", [], 1.0))

    def test_canvas_dashboard_completes_via_native_read_without_prompting(self):
        # Observed 2026-10-16: the dashboard lacked the old profile-link marker and its
        # navigation was offered as login controls. Canvas pages must never be prompts.
        page = Mock()
        page.url = "https://login.ufl.edu/idp/profile/SAML2/Redirect/SSO?execution=e1s3"
        ticks = [0]
        def tick(_):
            ticks[0] += 1
            if ticks[0] == 2:
                page.url = "https://ufl.instructure.com/login/saml"
            if ticks[0] == 3:
                page.url = "https://ufl.instructure.com/"
        page.wait_for_timeout.side_effect = tick
        page.locator.return_value.count.return_value = 0
        runtime = Mock()
        context = runtime.chromium.launch.return_value.new_context.return_value
        context.new_page.return_value = page
        context.storage_state.return_value = {"cookies": [], "origins": []}
        manager = Mock()
        manager.__enter__ = Mock(return_value=runtime)
        manager.__exit__ = Mock(return_value=False)
        args = SimpleNamespace(channel="msedge", browser=False, show_password=False)
        saving = "UNIVERSITY OF FLORIDA LOGIN SERVICE - SAVING SESSION INFORMATION..."
        clock = iter(range(0, 10_000))
        with patch("playwright.sync_api.sync_playwright", return_value=manager), \
             patch("tools.terminal_login.time.monotonic", side_effect=lambda: next(clock) * 2.0), \
             patch("tools.terminal_login.describe_controls", return_value=(saving, [])) as describe, \
             patch("tools.terminal_login.poll_key", return_value=None), \
             patch("tools.terminal_login.interact") as interact, \
             patch("tools.terminal_login.verify_and_save", side_effect=[(False, {"kind": "unauthenticated", "status": 401}),
                                                                       (True, {"kind": "json", "status": 200})]) as verify, \
             patch("tools.terminal_login.write_report"), \
             patch("builtins.input", side_effect=AssertionError("Unexpected blocking prompt")), \
             patch("sys.stdout", new_callable=io.StringIO) as out:
            self.assertEqual(browser_login(args, None), 0)
        interact.assert_not_called()
        self.assertEqual(verify.call_count, 2)
        # Controls were only collected on UF pages, never on Canvas.
        self.assertEqual(describe.call_count, 1)
        printed = out.getvalue()
        self.assertIn("Finishing sign-in", printed)
        self.assertNotIn("SAVING SESSION INFORMATION", printed)

    def test_content_read_while_the_page_moves_is_discarded(self):
        # Observed 2026-10-20: the URL was read before a slow text read, so the
        # Canvas dashboard was shown as a prompt under the previous page's URL.
        page = Mock()
        page.url = "https://login.ufl.edu/idp/profile/SAML2/Redirect/SSO?execution=e1s3"
        page.locator.return_value.count.return_value = 0
        ticks = [0]
        def tick(_):
            ticks[0] += 1
        page.wait_for_timeout.side_effect = tick
        def describe(_):
            if ticks[0] == 1:
                page.url = "https://ufl.instructure.com/"
                return "Dashboard\nCourses", controls("Dashboard", "Courses")
            raise AssertionError("Canvas pages must not be read as prompts")
        runtime = Mock()
        context = runtime.chromium.launch.return_value.new_context.return_value
        context.new_page.return_value = page
        context.storage_state.return_value = {"cookies": [], "origins": []}
        manager = Mock()
        manager.__enter__ = Mock(return_value=runtime)
        manager.__exit__ = Mock(return_value=False)
        args = SimpleNamespace(channel="msedge", browser=False, show_password=False)
        with patch("playwright.sync_api.sync_playwright", return_value=manager), \
             patch("tools.terminal_login.describe_controls", side_effect=describe), \
             patch("tools.terminal_login.poll_key", return_value=None), \
             patch("tools.terminal_login.interact") as interact, \
             patch("tools.terminal_login.verify_and_save", return_value=(True, {"kind": "json", "status": 200})), \
             patch("tools.terminal_login.write_report"), \
             patch("builtins.input", side_effect=AssertionError("Unexpected blocking prompt")), \
             patch("sys.stdout", new_callable=io.StringIO) as out:
            self.assertEqual(browser_login(args, None), 0)
        interact.assert_not_called()
        self.assertNotIn("Courses", out.getvalue())


if __name__ == "__main__":
    unittest.main()
