"""boots the raspberry pi image in qemu and follows its serial console.

qemu emulates the board and its sd card, but neither the wifi chip nor any
other network device. so the image is told to serve on the loopback
interface, and the tests check what it reports on the way there. the access
point itself can only be checked on hardware.
"""

import pathlib
import re
import shutil
import subprocess
import tempfile
import threading
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
BUILD = ROOT / "target" / "rpi"
IMAGE = BUILD / "oasis-rpi4.img"
BOOT_SECS = 90
SSID = "base camp"
# the name that qemu's pi gives the serial port, and the settings under test
CMDLINE = f'console=ttyAMA1,115200 panic=10 oasis.interface=lo oasis.ssid="{SSID}"'
SERVING = f'oasis "{SSID}" serving http://10.0.0.1/ on lo'
ROOM = re.compile(r"oasis: room for (\d+) MB of board, (\d+) accounts, (\d+) mailboxes")
MEGABYTE = 1024 * 1024
MAILBOX_BYTES = 64_000


def boot(card):
    """runs the image until it serves, and returns its console output."""
    command = [
        "qemu-system-aarch64",
        *("-M", "raspi4b", "-nographic", "-no-reboot", "-monitor", "none", "-serial", "stdio"),
        *("-kernel", BUILD / "kernel8.img", "-dtb", BUILD / "bcm2711-rpi-4-b.dtb"),
        *("-initrd", BUILD / "initramfs.cpio.gz", "-append", CMDLINE),
        *("-drive", f"file={card},if=sd,format=raw"),
    ]
    qemu = subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, errors="replace"
    )
    watchdog = threading.Timer(BOOT_SECS, qemu.kill)
    watchdog.start()
    lines = []
    for line in qemu.stdout:
        lines.append(line.rstrip())
        if SERVING in line or "Rebooting in" in line:
            break
    watchdog.cancel()
    qemu.kill()
    qemu.wait()
    return lines


class BootTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.card = pathlib.Path(cls.tmp.name) / "card.img"
        shutil.copyfile(IMAGE, cls.card)
        # qemu wants the size of an sd card to be a power of two
        with cls.card.open("r+b") as card:
            card.truncate(1 << (IMAGE.stat().st_size - 1).bit_length())
        cls.console = boot(cls.card)
        cls.output = "\n".join(cls.console)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_image_boots_into_the_portal(self):
        self.assertIn(SERVING, self.output, "\n".join(self.console[-30:]))
        self.assertNotIn("Kernel panic", self.output)

    def test_settings_come_from_the_kernel_command_line(self):
        self.assertIn(f'oasis "{SSID}" serving', self.output, "a quoted value keeps its space")
        self.assertIn(" on lo", self.output)

    def test_wifi_driver_is_loaded_with_what_it_depends_on(self):
        self.assertIn("cfg80211: Loading compiled-in X.509 certificates", self.output)
        self.assertIn("registered new interface driver brcmfmac", self.output)

    def test_data_partition_is_found_and_mounted(self):
        self.assertRegex(self.output, r"EXT4-fs \(mmcblk\dp2\): mounted filesystem")

    def test_limits_grow_with_the_data_partition(self):
        match = ROOM.search(self.output)
        self.assertIsNotNone(match, self.output[-2000:])
        board_mb, accounts, mailboxes = map(int, match.groups())
        partition = IMAGE.stat().st_size - 65 * MEGABYTE
        self.assertGreater(board_mb, partition // MEGABYTE // 3, "about half of the partition")
        self.assertLess(board_mb, partition // MEGABYTE // 2)
        for_mail = board_mb * MEGABYTE // 2 // MAILBOX_BYTES
        self.assertAlmostEqual(mailboxes, for_mail, delta=for_mail // 50, msg="half as much for mail")
        self.assertEqual(accounts, 10_000)

    def test_https_listener_finds_its_certificate(self):
        self.assertNotIn("no https", self.output)


if __name__ == "__main__":
    unittest.main()
