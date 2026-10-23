-- `scripts/vm-test.sh run X.app --osascript scripts/vm-hooks/press-return.applescript`:
-- bring Wine to the front and press Return, i.e. accept the default button of
-- a first-run dialog (Carmageddon TDR 2000's "Hardware Capabilities").
-- A System Events `click at` reaches Wine's window but doesn't press the button.
tell application "System Events"
	set frontmost of (first process whose name is "wine") to true
	delay 1
	key code 36
end tell
