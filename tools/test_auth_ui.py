import io
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from tools.auth_ui import duo_view
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
        self.assertIn("123", view.lines[0])
        self.assertNotIn("Loading", " ".join(view.lines))
        self.assertNotIn("0123", " ".join(view.lines))
        self.assertEqual(view.actions, (("o", 0, "Other methods"),))

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
        self.assertNotIn("0123", " ".join(view.lines))

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
             patch("tools.terminal_login.poll_key", side_effect=[None, None, "y", None]), \
             patch("tools.terminal_login.interact") as interact, \
             patch("tools.terminal_login.verify_and_save", return_value=(True, {"kind": "json", "status": 200})), \
             patch("tools.terminal_login.write_report"), \
             patch("builtins.input", side_effect=AssertionError("Unexpected blocking prompt")), \
             patch("sys.stdout", new_callable=io.StringIO):
            self.assertEqual(browser_login(args, None), 0)
            interact.assert_called_once()
            self.assertEqual(interact.call_args.args[3], "1")
        context.close.assert_called_once()


if __name__ == "__main__":
    unittest.main()
