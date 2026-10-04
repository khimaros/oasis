"""assembles the sd card image for the raspberry pi 4.

the card has two partitions. the first is the FAT one that the pi boots from:
its firmware, a stock raspberry pi kernel, and an initramfs. the initramfs
holds the oasis binary as /init, the wifi driver modules, and the firmware of
the wifi chip. the second partition is an empty ext4 for the data.

kernel, modules, and firmware are downloaded once into the cache directory.
they are pinned by commit and checked against the hashes in `rpi.lock`.
after changing a pin, remove the lock file and run once with
OASIS_RPI_RELOCK=1 to record the new hashes.
"""

import argparse
import gzip
import hashlib
import lzma
import os
import pathlib
import shutil
import struct
import subprocess
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent
LOCK_FILE = ROOT / "rpi.lock"
RAW = "https://raw.githubusercontent.com"
# tag 1.20260915 of the kernel and boot firmware
FIRMWARE = f"{RAW}/raspberrypi/firmware/6f0881cba8bea8ec24956a5718714730e9936ec5"
# branch trixie of the wifi chip firmware
WIFI_FIRMWARE = (
    f"{RAW}/RPi-Distro/firmware-nonfree/3bab0f823f5b53150b76aab77093adef6655b920/debian/added-firmware"
)
KERNEL_RELEASE = "6.18.50-v8+"
KERNEL, DEVICE_TREE, INITRAMFS = "kernel8.img", "bcm2711-rpi-4-b.dtb", "initramfs.cpio.gz"
BOOT_FILES = ["start4.elf", "fixup4.dat", DEVICE_TREE, KERNEL]
MODULES_DIR = f"modules/{KERNEL_RELEASE}"
WIRELESS = "kernel/drivers/net/wireless/broadcom/brcm80211"
# the wifi driver, what it depends on, and the vendor parts that it asks
# the kernel to load once it has identified the chip
MODULES = [
    "kernel/net/rfkill/rfkill.ko",
    "kernel/net/wireless/cfg80211.ko",
    f"{WIRELESS}/brcmutil/brcmutil.ko",
    f"{WIRELESS}/brcmfmac/brcmfmac.ko",
    f"{WIRELESS}/brcmfmac/wcc/brcmfmac-wcc.ko",
    f"{WIRELESS}/brcmfmac/cyw/brcmfmac-cyw.ko",
    f"{WIRELESS}/brcmfmac/bca/brcmfmac-bca.ko",
]
MODULE_PACKING = ".xz"
# where the driver looks for the firmware of the CYW43455, and where it is
# published. the "minimal" build leaves out features that an access point
# does not need and accepts more clients in exchange
CHIP_FIRMWARE = {
    "brcm/brcmfmac43455-sdio.bin": "cypress/cyfmac43455-sdio-minimal.bin",
    "brcm/brcmfmac43455-sdio.clm_blob": "cypress/cyfmac43455-sdio.clm_blob",
    "brcm/brcmfmac43455-sdio.txt": "brcm/brcmfmac43455-sdio.txt",
}
TLS_FILES = ["cert.pem", "key.pem"]
CONFIG = f"""\
arm_64bit=1
kernel={KERNEL}
initramfs {INITRAMFS} followkernel
enable_uart=1
disable_splash=1
boot_delay=0
"""
# a kernel that loses its init restarts the device
CMDLINE = "console=serial0,115200 console=tty1 panic=10"

SECTOR = 512
MEGABYTE = 1024 * 1024
BOOT_START = MEGABYTE
BOOT_BYTES = 64 * MEGABYTE
PARTITION_FAT32, PARTITION_LINUX = 0x0C, 0x83
# cylinder, head, and sector fields are unused with block addresses
NO_CHS = b"\xfe\xff\xff"
# the disk id and two reserved bytes come right before the four partitions
MBR_DISK_ID_AT, MBR_SIGNATURE = 440, b"\x55\xaa"
# fixed ids, so that the same inputs give the same image
DISK_ID = 0x0A515000
DATA_UUID = "0a515000-0000-4000-8000-000000000001"
# what the init looks for to find the data partition
DATA_LABEL = "oasis"
EPOCH = "0"
# a file takes at least a block, and the smallest files are mail segments
BYTES_PER_INODE = 4096
CPIO_MAGIC = "070701"
CPIO_TRAILER = "TRAILER!!!"
MODE_DIR, MODE_FILE, MODE_EXEC, MODE_LINK, MODE_CHAR = 0o040755, 0o100644, 0o100755, 0o120777, 0o020600
# major and minor numbers of the devices that init needs before it mounts
# /dev: the kernel opens the console for it. should that fail, rust falls
# back to the null device, and gives up without one
DEVICES = {"dev/console": (5, 1), "dev/null": (1, 3)}
SBIN = ":/sbin:/usr/sbin"


def locked():
    """sha256 by url, as recorded in the lock file."""
    lines = LOCK_FILE.read_text().splitlines() if LOCK_FILE.exists() else []
    return {url: digest for digest, url in (line.split() for line in lines)}


def fetch(url, cache):
    """the content behind `url`, downloaded once and checked against the lock."""
    path = cache / hashlib.sha256(url.encode()).hexdigest()
    if not path.exists():
        print(f"fetching {url}")
        with urllib.request.urlopen(url) as response:
            path.write_bytes(response.read())
    data = path.read_bytes()
    digest, expected = hashlib.sha256(data).hexdigest(), locked().get(url)
    if os.environ.get("OASIS_RPI_RELOCK"):
        with LOCK_FILE.open("a") as lock:
            lock.write(f"{digest}  {url}\n")
    elif digest != expected:
        path.unlink()
        raise SystemExit(f"{url}: sha256 is {digest}, the lock says {expected}")
    return data


def cpio_entry(index, name, mode, data=b"", device=(0, 0)):
    """one member of a "newc" cpio archive, owned by root."""
    fields = [index, mode, 0, 0, 1, 0, len(data), 0, 0, *device, len(name) + 1, 0]
    head = (CPIO_MAGIC + "".join(f"{field:08x}" for field in fields) + name + "\0").encode()
    return head + b"\0" * (-len(head) % 4) + data + b"\0" * (-len(data) % 4)


def cpio(files):
    """a gzipped cpio archive of `files`: (mode, data) by path. directories
    are added as needed. a link has its target as data."""
    files = {**files, **{path: (MODE_CHAR, numbers) for path, numbers in DEVICES.items()}}
    parents = {str(parent) for path in files for parent in pathlib.PurePosixPath(path).parents}
    members = {**{path: (MODE_DIR, b"") for path in parents - {"."}}, **files}
    archive = b""
    for index, (path, (mode, data)) in enumerate(sorted(members.items()), 1):
        is_device = mode == MODE_CHAR
        archive += cpio_entry(index, path, mode, b"" if is_device else data, data if is_device else (0, 0))
    archive += cpio_entry(0, CPIO_TRAILER, 0)
    return gzip.compress(archive, mtime=0)


def module_files(cache):
    """the unpacked driver modules, and the dependency list cut down to them."""
    packed = [module + MODULE_PACKING for module in MODULES]
    listed = fetch(f"{FIRMWARE}/{MODULES_DIR}/modules.dep", cache).decode().splitlines()
    kept = [line.replace(MODULE_PACKING, "") for line in listed if line.split(":")[0] in packed]
    assert len(kept) == len(MODULES), kept
    files = {
        module: lzma.decompress(fetch(f"{FIRMWARE}/{MODULES_DIR}/{module}{MODULE_PACKING}", cache))
        for module in MODULES
    }
    return {**files, "modules.dep": "".join(line + "\n" for line in kept).encode()}


def initramfs(binary, tls_dir, cache):
    modules = {f"lib/{MODULES_DIR}/{path}": (MODE_FILE, data) for path, data in module_files(cache).items()}
    firmware = {
        f"lib/firmware/{path}": (MODE_FILE, fetch(f"{WIFI_FIRMWARE}/{source}", cache))
        for path, source in CHIP_FIRMWARE.items()
    }
    tls = {f"tls/{name}": (MODE_FILE, (tls_dir / name).read_bytes()) for name in TLS_FILES}
    init = {"init": (MODE_EXEC, binary.read_bytes()), "sbin/modprobe": (MODE_LINK, b"/init")}
    return cpio({**init, **modules, **firmware, **tls})


def run(tool, *args):
    path = shutil.which(tool, path=os.environ["PATH"] + SBIN)
    if not path:
        raise SystemExit(f"{tool} is needed to build the image")
    environment = {**os.environ, "SOURCE_DATE_EPOCH": EPOCH, "E2FSPROGS_FAKE_TIME": EPOCH}
    subprocess.run([path, *map(str, args)], check=True, env=environment, stdout=subprocess.DEVNULL)


def boot_partition(files, work):
    """a FAT file system that holds `files`."""
    image = work / "boot.fat"
    image.unlink(missing_ok=True)
    run(
        "mkfs.vfat",
        "-F",
        32,
        "-n",
        "OASIS",
        "-i",
        f"{DISK_ID:08x}",
        "--invariant",
        "-C",
        image,
        BOOT_BYTES // 1024,
    )
    for name, data in files.items():
        (work / name).write_bytes(data)
        run("mcopy", "-i", image, work / name, f"::{name}")
    return image.read_bytes()


def partition_table(partitions):
    """a master boot record for (type, first byte, bytes) partitions."""
    entries = b"".join(
        b"\0" + NO_CHS + bytes([kind]) + NO_CHS + struct.pack("<II", start // SECTOR, size // SECTOR)
        for kind, start, size in partitions
    )
    table = struct.pack("<IH", DISK_ID, 0) + entries
    return (
        b"\0" * MBR_DISK_ID_AT
        + table.ljust(SECTOR - MBR_DISK_ID_AT - len(MBR_SIGNATURE), b"\0")
        + MBR_SIGNATURE
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", type=pathlib.Path, required=True, help="the oasis-rpi executable")
    parser.add_argument("--tls", type=pathlib.Path, required=True, help="directory of cert.pem and key.pem")
    parser.add_argument("--cache", type=pathlib.Path, required=True, help="keeps downloads")
    parser.add_argument(
        "--out", type=pathlib.Path, required=True, help="directory for the image and its parts"
    )
    parser.add_argument("--data-mb", type=int, required=True, help="size of the data partition")
    parser.add_argument("--cmdline", default="", help="settings, appended to the kernel command line")
    args = parser.parse_args()
    args.cache.mkdir(parents=True, exist_ok=True)
    args.out.mkdir(parents=True, exist_ok=True)
    boot = {name: fetch(f"{FIRMWARE}/boot/{name}", args.cache) for name in BOOT_FILES}
    boot[INITRAMFS] = initramfs(args.binary, args.tls, args.cache)
    boot["config.txt"] = CONFIG.encode()
    boot["cmdline.txt"] = f"{CMDLINE} {args.cmdline}".strip().encode() + b"\n"
    data_start, data_bytes = BOOT_START + BOOT_BYTES, args.data_mb * MEGABYTE
    image = args.out / "oasis-rpi4.img"
    fat = boot_partition(boot, args.out)
    with image.open("wb") as file:
        file.truncate(data_start + data_bytes)
    options = f"offset={data_start},hash_seed={DATA_UUID},root_owner=0:0"
    run(
        "mke2fs",
        "-q",
        "-F",
        "-t",
        "ext4",
        "-i",
        BYTES_PER_INODE,
        "-m",
        0,
        "-L",
        DATA_LABEL,
        "-U",
        DATA_UUID,
        "-E",
        options,
        image,
        f"{data_bytes // 1024}k",
    )
    partitions = [(PARTITION_FAT32, BOOT_START, BOOT_BYTES), (PARTITION_LINUX, data_start, data_bytes)]
    with image.open("r+b") as file:
        file.write(partition_table(partitions))
        file.seek(BOOT_START)
        file.write(fat)
    print(f"wrote {image}")


if __name__ == "__main__":
    main()
