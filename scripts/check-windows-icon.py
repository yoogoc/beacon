#!/usr/bin/env python3
"""Verify every embedded Windows application icon against the source ICO."""

import argparse
import ctypes
from ctypes import wintypes
from pathlib import Path
import struct
import sys


def check_icon(binary, icon):
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.LoadLibraryExW.argtypes = [wintypes.LPCWSTR, wintypes.HANDLE, wintypes.DWORD]
    kernel32.LoadLibraryExW.restype = wintypes.HMODULE
    kernel32.FreeLibrary.argtypes = [wintypes.HMODULE]
    kernel32.FreeLibrary.restype = wintypes.BOOL
    kernel32.FindResourceW.argtypes = [wintypes.HMODULE, ctypes.c_void_p, ctypes.c_void_p]
    kernel32.FindResourceW.restype = wintypes.HANDLE
    kernel32.SizeofResource.argtypes = [wintypes.HMODULE, wintypes.HANDLE]
    kernel32.SizeofResource.restype = wintypes.DWORD
    kernel32.LoadResource.argtypes = [wintypes.HMODULE, wintypes.HANDLE]
    kernel32.LoadResource.restype = wintypes.HANDLE
    kernel32.LockResource.argtypes = [wintypes.HANDLE]
    kernel32.LockResource.restype = ctypes.c_void_p

    # Load resources without executing the application, including other CPU
    # architectures. LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE.
    module = kernel32.LoadLibraryExW(str(binary.resolve()), None, 0x02 | 0x20)
    if not module:
        raise ctypes.WinError(ctypes.get_last_error())

    def resource(name, kind):
        entry = kernel32.FindResourceW(module, name, kind)
        if not entry:
            raise ValueError(f"Missing icon resource: type={kind}, id={name}")
        size = kernel32.SizeofResource(module, entry)
        loaded = kernel32.LoadResource(module, entry)
        address = kernel32.LockResource(loaded) if loaded else None
        if not size or not address:
            raise ValueError(f"Cannot read icon resource: type={kind}, id={name}")
        return ctypes.string_at(address, size)

    try:
        expected = icon.read_bytes()
        group = resource(1, 14)  # RT_GROUP_ICON; ID 1 is declared in beacon.rc.
        header = struct.unpack_from("<HHH", expected)
        if header[:2] != (0, 1) or header[2] == 0:
            raise ValueError("Source file is not a valid ICO")
        if struct.unpack_from("<HHH", group) != header:
            raise ValueError("Embedded icon sizes do not match the source ICO")
        for index in range(header[2]):
            source = struct.unpack_from("<BBBBHHII", expected, 6 + 16 * index)
            embedded = struct.unpack_from("<BBBBHHIH", group, 6 + 14 * index)
            if embedded[:7] != source[:7]:
                raise ValueError(f"Icon entry {index} does not match the source ICO")
            image_size, image_offset = source[6:]
            image = expected[image_offset:image_offset + image_size]
            if len(image) != image_size or resource(embedded[7], 3) != image:
                raise ValueError(f"Icon image {index} does not match the source ICO")
        print(f"Verified {header[2]} embedded application icons in {binary}")
    finally:
        kernel32.FreeLibrary(module)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--icon", type=Path,
                        default=Path(__file__).resolve().parents[1] / "assets/app-icon/beacon.ico")
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("This check requires Windows resource-loading APIs")
    try:
        check_icon(args.binary, args.icon)
    except (OSError, ValueError, struct.error) as error:
        parser.exit(1, f"Windows icon verification failed: {error}\n")


if __name__ == "__main__":
    main()
