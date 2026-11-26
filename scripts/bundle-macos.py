"""Package the Rust executable using only the Python standard library."""
from pathlib import Path
import plistlib
import shutil

root = Path(__file__).resolve().parent.parent
bundle = root / "app/build/release/recho.app/Contents"
(bundle / "MacOS").mkdir(parents=True, exist_ok=True)
(bundle / "Resources").mkdir(exist_ok=True)
for filename in ["NOTICE", "SOURCES.md", "Geist-OFL.txt"]:
    shutil.copy2(root / "desktop/assets/fonts" / filename, bundle / "Resources" / filename)
shutil.copy2(root / "desktop/assets/icons/recho.icns", bundle / "Resources/recho.icns")
shutil.copy2(root / "app/build/release/recho", bundle / "MacOS/recho")
with (bundle / "Info.plist").open("wb") as output:
    plistlib.dump({
        "CFBundleExecutable": "recho",
        "CFBundleIdentifier": "io.github.jyy2luck.recho",
        "CFBundleName": "recho",
        "CFBundleDisplayName": "recho",
        "CFBundleIconFile": "recho.icns",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": "0.1.0",
        "CFBundleVersion": "1",
        "LSMinimumSystemVersion": "12.0",
        "NSHighResolutionCapable": True,
        "NSSupportsAutomaticGraphicsSwitching": True,
    }, output)
