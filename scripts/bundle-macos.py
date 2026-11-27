"""Package the Rust executable using only the Python standard library."""
import argparse
from pathlib import Path
import plistlib
import shutil

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("profile", nargs="?", choices=("debug", "release"), default="release")
args = parser.parse_args()

root = Path(__file__).resolve().parent.parent
build = root / "app/build" / args.profile
bundle = build / "recho.app/Contents"
(bundle / "MacOS").mkdir(parents=True, exist_ok=True)
(bundle / "Resources").mkdir(exist_ok=True)
for filename in ["NOTICE", "SOURCES.md", "Geist-OFL.txt"]:
    shutil.copy2(root / "desktop/assets/fonts" / filename, bundle / "Resources" / filename)
shutil.copy2(root / "desktop/assets/icons/recho.icns", bundle / "Resources/recho.icns")
shutil.copy2(build / "recho", bundle / "MacOS/recho")
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
