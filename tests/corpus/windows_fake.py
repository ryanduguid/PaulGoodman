# Copyright 2026 Ryan Duguid
# SPDX-License-Identifier: Apache-2.0

"""Run the Windows publication transaction on POSIX through a faithful fake.

The live-evidence exporter reaches kernel32, ntdll and msvcrt through ctypes,
so the Ubuntu coverage leg could not run 182 of its lines. `windows` leaves a
test untouched on Windows; elsewhere it runs the test with:

- `ctypes.WinDLL` returning libraries whose functions act on real POSIX
  descriptors. A handle is a descriptor: CreateFileW opens without following
  a final link, NtOpenFile opens one name relative to its root handle and
  honours the UNICODE_STRING length in UTF-16 units, SetFileInformationByHandle
  deletes only the object the handle holds, and NtSetInformationFile renames the
  held directory under its root handle and never replaces a destination.
- an `msvcrt` whose descriptors are those same handles.
- the exporter's `os` seen as Windows (`os.name == "nt"`), so patches of
  `export_module.os` reach the exporter alone, as they would on Windows.

Windows share modes are not emulated: a test that relies on a held handle
refusing another process's rename or delete stays Windows-only.
"""

from __future__ import annotations

import contextlib
import ctypes
import errno
import functools
import os
import stat
import sys
import types
from ctypes import wintypes
from typing import Any, Callable, Iterator, TypeVar
from unittest import mock

import fadden.export_live_evidence_bundles as export_module

_INVALID_HANDLE = wintypes.HANDLE(-1).value
_STATUS_OBJECT_NAME_NOT_FOUND = -0x3FFFFFCC  # 0xC0000034 as a signed LONG
_STATUS_OBJECT_NAME_COLLISION = -0x3FFFFFCB  # 0xC0000035
_STATUS_FILE_IS_A_DIRECTORY = -0x3FFFFF46  # 0xC00000BA
_STATUS_NOT_A_DIRECTORY = -0x3FFFFEFD  # 0xC0000103
_STATUS_INVALID_HANDLE = -0x3FFFFFF8  # 0xC0000008
_ERROR_INVALID_HANDLE = 6
# O_PATH opens a final symbolic link itself, as FILE_FLAG_OPEN_REPARSE_POINT
# opens a reparse point; hosts without it open the target for reading.
_HOLD = getattr(os, "O_PATH", os.O_RDONLY) | getattr(os, "O_NOFOLLOW", 0)

_T = TypeVar("_T")
_REAL_WINDLL = getattr(ctypes, "WinDLL", None)
_ACTIVE: dict[str, Any] = {}


def _value(handle: Any) -> int:
    value = getattr(handle, "value", handle)
    return -1 if value is None else int(value)


def _target(reference: Any) -> Any:
    """The object behind ctypes.byref(), as the real call would write it."""

    return getattr(reference, "_obj", reference)


class _Handles:
    """Each open handle's descriptor and the path it was opened by."""

    def __init__(self) -> None:
        self.paths: dict[int, str] = {}
        self.last_error = 0

    def add(self, descriptor: int, path: str) -> int:
        self.paths[descriptor] = path
        return descriptor

    def path(self, handle: Any) -> str | None:
        """The handle's path, only while that path still names the held object."""

        descriptor = _value(handle)
        path = self.paths.get(descriptor)
        if path is None:
            return None
        try:
            held, named = os.fstat(descriptor), os.lstat(path)
        except OSError:
            return None
        return path if os.path.samestat(held, named) else None


class _Function:
    """A library export that accepts argtypes and restype like a ctypes one."""

    def __init__(self, call: Callable[..., Any]) -> None:
        self._call = call
        self.argtypes: list[Any] | None = None
        self.restype: Any = None

    def __call__(self, *arguments: Any) -> Any:
        return self._call(*arguments)


class _Kernel32:
    def __init__(self, handles: _Handles) -> None:
        self._handles = handles
        for name in (
            "CreateFileW",
            "GetFileInformationByHandle",
            "CloseHandle",
            "GetCurrentProcess",
            "DuplicateHandle",
            "SetFileInformationByHandle",
        ):
            setattr(self, name, _Function(getattr(self, f"_{name}")))

    def _CreateFileW(self, path: str, *_arguments: Any) -> int:
        try:
            return self._handles.add(os.open(path, _HOLD), path)
        except OSError:
            return _INVALID_HANDLE

    def _GetFileInformationByHandle(self, handle: Any, details: Any) -> int:
        try:
            observed = os.fstat(_value(handle))
        except OSError:
            return 0
        attributes = 0x10 if stat.S_ISDIR(observed.st_mode) else 0x80
        if stat.S_ISLNK(observed.st_mode):
            attributes |= 0x400
        information = _target(details)
        information.file_attributes = attributes
        information.number_of_links = observed.st_nlink
        return 1

    def _CloseHandle(self, handle: Any) -> int:
        descriptor = _value(handle)
        try:
            os.close(descriptor)
        except OSError:
            self._handles.last_error = _ERROR_INVALID_HANDLE
            return 0
        self._handles.paths.pop(descriptor, None)
        return 1

    def _GetCurrentProcess(self) -> int:
        return _INVALID_HANDLE

    def _DuplicateHandle(
        self, _source: Any, handle: Any, _target_process: Any, duplicate: Any, *_rest: Any
    ) -> int:
        descriptor = _value(handle)
        try:
            copy = os.dup(descriptor)
        except OSError:
            self._handles.last_error = _ERROR_INVALID_HANDLE
            return 0
        self._handles.add(copy, self._handles.paths.get(descriptor, ""))
        _target(duplicate).value = copy
        return 1

    def _SetFileInformationByHandle(
        self, handle: Any, information_class: int, information: Any, _size: int
    ) -> int:
        path = self._handles.path(handle)
        if information_class != 4 or not _target(information).delete_file or path is None:
            self._handles.last_error = _ERROR_INVALID_HANDLE
            return 0
        try:
            if stat.S_ISDIR(os.fstat(_value(handle)).st_mode):
                os.rmdir(path)
            else:
                os.unlink(path)
        except OSError:
            return 0
        return 1


class _Ntdll:
    def __init__(self, handles: _Handles) -> None:
        self._handles = handles
        self.NtOpenFile = _Function(self._NtOpenFile)
        self.NtSetInformationFile = _Function(self._NtSetInformationFile)

    def _NtOpenFile(
        self, handle: Any, _access: int, attributes: Any, _status: Any, _share: int, options: int
    ) -> int:
        object_attributes = _target(attributes)
        root = _value(object_attributes.root_directory)
        unicode_name = object_attributes.object_name.contents
        encoded = (unicode_name.buffer or "").encode("utf-16-le", "surrogatepass")
        name = encoded[: unicode_name.length].decode("utf-16-le", "surrogatepass")
        directory = bool(options & export_module._FILE_DIRECTORY_FILE)
        try:
            descriptor = os.open(
                name, _HOLD if directory else os.O_RDONLY | os.O_NOFOLLOW, dir_fd=root
            )
        except OSError as exc:
            if exc.errno == errno.EBADF:
                return _STATUS_INVALID_HANDLE
            return _STATUS_OBJECT_NAME_NOT_FOUND
        mode = os.fstat(descriptor).st_mode
        refusal = (
            _STATUS_NOT_A_DIRECTORY
            if directory and not (stat.S_ISDIR(mode) or stat.S_ISLNK(mode))
            else _STATUS_FILE_IS_A_DIRECTORY
            if not directory and stat.S_ISDIR(mode)
            else 0
        )
        if refusal:
            os.close(descriptor)
            return refusal
        root_path = self._handles.paths.get(root, "")
        self._handles.add(descriptor, os.path.join(root_path, name))
        _target(handle).value = descriptor
        return 0

    def _NtSetInformationFile(
        self, handle: Any, _status: Any, buffer: Any, size: int, information_class: int
    ) -> int:
        source = self._handles.path(handle)
        rename = ctypes.cast(buffer, ctypes.POINTER(export_module._FileRenameInfo)).contents
        offset = export_module._FileRenameInfo.file_name_length.offset + ctypes.sizeof(
            wintypes.DWORD
        )
        length = rename.file_name_length
        if information_class != 10 or source is None or offset + length > size:
            return _STATUS_INVALID_HANDLE
        name = ctypes.string_at(ctypes.addressof(buffer) + offset, length).decode(
            "utf-16-le", "surrogatepass"
        )
        root = self._handles.path(rename.root_directory)
        if root is None:
            return _STATUS_INVALID_HANDLE
        destination = os.path.join(root, name)
        if rename.replace_if_exists or os.path.lexists(destination):
            return _STATUS_OBJECT_NAME_COLLISION
        os.rename(source, destination)
        self._handles.paths[_value(handle)] = destination
        return 0


class _WindowsOs:
    """The exporter's view of `os` on Windows; everything else is the real module."""

    name = "nt"
    O_BINARY = 0

    def __getattr__(self, attribute: str) -> Any:
        return getattr(os, attribute)


@contextlib.contextmanager
def fake_windows() -> Iterator[_Handles]:
    """Install the fake for the exporter until the context exits."""

    handles = _Handles()
    libraries = {"kernel32": _Kernel32(handles), "ntdll": _Ntdll(handles)}
    _ACTIVE.update(libraries)
    msvcrt = types.ModuleType("msvcrt")
    msvcrt.open_osfhandle = lambda handle, _flags: _value(handle)  # type: ignore[attr-defined]
    msvcrt.get_osfhandle = lambda descriptor: descriptor  # type: ignore[attr-defined]
    with (
        mock.patch.object(
            ctypes, "WinDLL", side_effect=lambda name, **_: libraries[name], create=True
        ),
        mock.patch.object(ctypes, "get_last_error", lambda: handles.last_error, create=True),
        mock.patch.dict(sys.modules, {"msvcrt": msvcrt}),
        mock.patch.object(export_module, "os", _WindowsOs()),
    ):
        try:
            yield handles
        finally:
            _ACTIVE.clear()


def library(name: str) -> Any:
    """The unpatched library: the real one on Windows, else the installed fake's."""

    if os.name == "nt":
        return _REAL_WINDLL(name, use_last_error=True)
    return _ACTIVE[name]


def windows(target: _T) -> _T:
    """Run a test, or every test of a class, on Windows or under the fake."""

    if os.name == "nt":
        return target
    if isinstance(target, type):
        for name, value in list(vars(target).items()):
            if name.startswith("test") and callable(value):
                setattr(target, name, windows(value))
        return target

    @functools.wraps(target)  # type: ignore[arg-type]
    def run(case: Any, *arguments: Any, **keywords: Any) -> Any:
        stack = contextlib.ExitStack()
        stack.enter_context(fake_windows())
        # Registered first, so it outlasts the test's own cleanups.
        case.addCleanup(stack.close)
        return target(case, *arguments, **keywords)  # type: ignore[operator]

    return run  # type: ignore[return-value]
