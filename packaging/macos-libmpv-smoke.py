"""Load a libmpv built by packaging/macos-libmpv-build.sh and check that it
works as Patina needs it to, through mpv's own client API (ctypes, nothing
to compile):

    python3 -I packaging/macos-libmpv-smoke.py <libmpv.2.dylib> [--arch <arch>]

It starts a core with no video or audio output and checks
  - the versions: mpv's, FFmpeg's and libplacebo's are the ones pinned in
    macos-libmpv.env and bundled-tools.env, beside this file;
  - that FFmpeg has dav1d's decoder (AV1), the https protocol (TLS, through
    OpenSSL), and the png, mjpeg and libwebp encoders (screenshots);
  - that Lua runs, and is LuaJIT: a script it loads reports jit.version;
  - that audio decodes and plays to its end: a WAV it writes, and a sine
    from libavfilter through av://lavfi (libavdevice), which is what
    Patina's tests play;
and prints FFmpeg's, libass's and libplacebo's versions and mpv's
configuration. It reports everything that fails, not just the first, and
exits 1 if anything did.

--arch is the architecture this Python has to be running as. Run under
Rosetta (`arch -x86_64 /usr/bin/python3 ...`) it loads the x86_64 half of a
universal libmpv, and --arch x86_64 makes sure it really did.

Written for the Python 3.9 that Xcode's /usr/bin/python3 is, and run with
-I, so nothing from the environment or the current directory is imported.
"""

import ctypes
import math
import os
import platform
import struct
import sys
import tempfile
import time
import wave

HERE = os.path.dirname(os.path.abspath(__file__))

MPV_FORMAT_STRING = 1
MPV_FORMAT_FLAG = 3
MPV_FORMAT_INT64 = 4
MPV_FORMAT_DOUBLE = 5
MPV_FORMAT_NODE = 6
MPV_FORMAT_NODE_ARRAY = 7
MPV_FORMAT_NODE_MAP = 8

MPV_EVENT_NONE = 0
MPV_EVENT_SHUTDOWN = 1
MPV_EVENT_LOG_MESSAGE = 2
MPV_EVENT_END_FILE = 7
MPV_EVENT_FILE_LOADED = 8

MPV_END_FILE_REASON_EOF = 0
END_FILE_REASONS = {0: "eof", 2: "stop", 3: "quit", 4: "error", 5: "redirect"}


class MpvNodeList(ctypes.Structure):
    pass


class MpvNodeUnion(ctypes.Union):
    _fields_ = [
        ("string", ctypes.c_char_p),
        ("flag", ctypes.c_int),
        ("int64", ctypes.c_int64),
        ("double_", ctypes.c_double),
        ("list", ctypes.POINTER(MpvNodeList)),
        ("ba", ctypes.c_void_p),
    ]


class MpvNode(ctypes.Structure):
    _fields_ = [("u", MpvNodeUnion), ("format", ctypes.c_int)]


MpvNodeList._fields_ = [
    ("num", ctypes.c_int),
    ("values", ctypes.POINTER(MpvNode)),
    ("keys", ctypes.POINTER(ctypes.c_char_p)),
]


class MpvEvent(ctypes.Structure):
    _fields_ = [
        ("event_id", ctypes.c_int),
        ("error", ctypes.c_int),
        ("reply_userdata", ctypes.c_uint64),
        ("data", ctypes.c_void_p),
    ]


class MpvEventEndFile(ctypes.Structure):
    _fields_ = [("reason", ctypes.c_int), ("error", ctypes.c_int)]


class MpvEventLogMessage(ctypes.Structure):
    _fields_ = [
        ("prefix", ctypes.c_char_p),
        ("level", ctypes.c_char_p),
        ("text", ctypes.c_char_p),
        ("log_level", ctypes.c_int),
    ]


def pins(name):
    """NAME=value lines from a pin file beside this one."""
    values = {}
    with open(os.path.join(HERE, name), encoding="utf-8") as file:
        for line in file:
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                key, value = line.split("=", 1)
                values[key] = value
    return values


def node_value(node):
    if node.format == MPV_FORMAT_STRING:
        return node.u.string.decode("utf-8", "replace")
    if node.format == MPV_FORMAT_FLAG:
        return bool(node.u.flag)
    if node.format == MPV_FORMAT_INT64:
        return node.u.int64
    if node.format == MPV_FORMAT_DOUBLE:
        return node.u.double_
    if node.format == MPV_FORMAT_NODE_ARRAY:
        items = node.u.list.contents
        return [node_value(items.values[i]) for i in range(items.num)]
    if node.format == MPV_FORMAT_NODE_MAP:
        items = node.u.list.contents
        return {
            items.keys[i].decode("utf-8", "replace"): node_value(items.values[i])
            for i in range(items.num)
        }
    return None


class Mpv:
    def __init__(self, path):
        lib = ctypes.CDLL(path)
        self.lib = lib
        lib.mpv_client_api_version.restype = ctypes.c_ulong
        lib.mpv_client_api_version.argtypes = []
        lib.mpv_create.restype = ctypes.c_void_p
        lib.mpv_create.argtypes = []
        lib.mpv_initialize.restype = ctypes.c_int
        lib.mpv_initialize.argtypes = [ctypes.c_void_p]
        lib.mpv_terminate_destroy.restype = None
        lib.mpv_terminate_destroy.argtypes = [ctypes.c_void_p]
        lib.mpv_set_option_string.restype = ctypes.c_int
        lib.mpv_set_option_string.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p]
        lib.mpv_get_property_string.restype = ctypes.c_void_p
        lib.mpv_get_property_string.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.mpv_get_property.restype = ctypes.c_int
        lib.mpv_get_property.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_void_p,
        ]
        lib.mpv_free.restype = None
        lib.mpv_free.argtypes = [ctypes.c_void_p]
        lib.mpv_free_node_contents.restype = None
        lib.mpv_free_node_contents.argtypes = [ctypes.POINTER(MpvNode)]
        lib.mpv_command.restype = ctypes.c_int
        lib.mpv_command.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_char_p)]
        lib.mpv_wait_event.restype = ctypes.POINTER(MpvEvent)
        lib.mpv_wait_event.argtypes = [ctypes.c_void_p, ctypes.c_double]
        lib.mpv_request_log_messages.restype = ctypes.c_int
        lib.mpv_request_log_messages.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        lib.mpv_error_string.restype = ctypes.c_char_p
        lib.mpv_error_string.argtypes = [ctypes.c_int]
        self.handle = None

    def error(self, code):
        return self.lib.mpv_error_string(code).decode()

    def create(self, options):
        self.handle = self.lib.mpv_create()
        if not self.handle:
            raise RuntimeError("mpv_create returned NULL")
        for name, value in options:
            code = self.lib.mpv_set_option_string(self.handle, name.encode(), value.encode())
            if code < 0:
                raise RuntimeError("could not set %s=%s: %s" % (name, value, self.error(code)))
        self.lib.mpv_request_log_messages(self.handle, b"warn")
        code = self.lib.mpv_initialize(self.handle)
        if code < 0:
            raise RuntimeError("mpv_initialize failed: %s" % self.error(code))

    def destroy(self):
        if self.handle:
            self.lib.mpv_terminate_destroy(self.handle)
            self.handle = None

    def get_string(self, name):
        pointer = self.lib.mpv_get_property_string(self.handle, name.encode())
        if not pointer:
            return None
        try:
            return ctypes.string_at(pointer).decode("utf-8", "replace")
        finally:
            self.lib.mpv_free(pointer)

    def get_node(self, name):
        node = MpvNode()
        code = self.lib.mpv_get_property(self.handle, name.encode(), MPV_FORMAT_NODE, ctypes.byref(node))
        if code < 0:
            raise RuntimeError("could not get %s: %s" % (name, self.error(code)))
        try:
            return node_value(node)
        finally:
            self.lib.mpv_free_node_contents(ctypes.byref(node))

    def get_node_or_none(self, name):
        node = MpvNode()
        code = self.lib.mpv_get_property(self.handle, name.encode(), MPV_FORMAT_NODE, ctypes.byref(node))
        if code < 0:
            return None
        try:
            return node_value(node)
        finally:
            self.lib.mpv_free_node_contents(ctypes.byref(node))

    def command(self, *args):
        argv = (ctypes.c_char_p * (len(args) + 1))()
        for i, arg in enumerate(args):
            argv[i] = arg.encode()
        argv[len(args)] = None
        code = self.lib.mpv_command(self.handle, argv)
        if code < 0:
            raise RuntimeError("%s failed: %s" % (" ".join(args), self.error(code)))

    def events(self, timeout, wait=0.25):
        """Yields (event id, event) until timeout seconds have passed,
        printing mpv's warnings and errors as they come."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            event = self.lib.mpv_wait_event(self.handle, wait).contents
            if event.event_id == MPV_EVENT_NONE:
                continue
            if event.event_id == MPV_EVENT_LOG_MESSAGE:
                message = ctypes.cast(event.data, ctypes.POINTER(MpvEventLogMessage)).contents
                print(
                    "  mpv %s [%s]: %s"
                    % (
                        message.level.decode(),
                        message.prefix.decode(),
                        message.text.decode("utf-8", "replace").rstrip(),
                    )
                )
                continue
            yield event.event_id, event


def play_to_end(mpv, url, what):
    """Plays url and returns None if it ends at its end, or what went
    wrong."""
    try:
        mpv.command("loadfile", url, "replace")
    except RuntimeError as error:
        return str(error)
    loaded = False
    for event_id, event in mpv.events(30):
        if event_id == MPV_EVENT_FILE_LOADED:
            loaded = True
        elif event_id == MPV_EVENT_END_FILE:
            end = ctypes.cast(event.data, ctypes.POINTER(MpvEventEndFile)).contents
            if end.reason == MPV_END_FILE_REASON_EOF and loaded:
                return None
            return "%s ended with reason %s (%s)%s" % (
                what,
                END_FILE_REASONS.get(end.reason, end.reason),
                mpv.error(end.error) if end.error else "no error",
                "" if loaded else ", without loading",
            )
        elif event_id == MPV_EVENT_SHUTDOWN:
            return "the core shut down while playing %s" % what
    return "%s did not finish playing within 30 s" % what


def write_wav(path):
    """Half a second of a 440 Hz tone, 16-bit mono at 48 kHz."""
    rate = 48000
    frames = bytearray()
    for i in range(rate // 2):
        frames += struct.pack("<h", int(8000 * math.sin(2 * math.pi * 440 * i / rate)))
    with wave.open(path, "wb") as file:
        file.setnchannels(1)
        file.setsampwidth(2)
        file.setframerate(rate)
        file.writeframes(bytes(frames))


def main(argv):
    # Line by line, so what this prints and what mpv logs come in order.
    sys.stdout.reconfigure(line_buffering=True)
    arch = None
    args = list(argv)
    if len(args) == 3 and args[1] == "--arch":
        arch = args.pop(2)
        args.pop(1)
    if len(args) != 1:
        print("usage: python3 -I macos-libmpv-smoke.py <libmpv.2.dylib> [--arch <arch>]", file=sys.stderr)
        return 2
    path = os.path.abspath(args[0])

    failures = []

    def check(ok, message):
        if not ok:
            failures.append(message)
            print("FAIL: " + message)

    machine = platform.machine()
    print("Python %s on %s (%s)" % (platform.python_version(), machine, sys.executable))
    if arch is not None:
        check(machine == arch, "this Python runs as %s, not %s" % (machine, arch))

    env = pins("macos-libmpv.env")
    tools = pins("bundled-tools.env")
    want_mpv = "mpv v" + env["MPV_VERSION"]
    want_ffmpeg = tools["FFMPEG_VERSION"]
    want_libplacebo = "v" + env["LIBPLACEBO_VERSION"]

    try:
        mpv = Mpv(path)
    except (OSError, AttributeError) as error:
        print("could not load %s as libmpv: %s" % (path, error), file=sys.stderr)
        return 1
    version = mpv.lib.mpv_client_api_version()
    print("%s: client API %d.%d" % (path, version >> 16, version & 0xFFFF))
    check(version >> 16 == 2, "client API %d.x, but Patina needs 2.x" % (version >> 16))

    scratch = tempfile.mkdtemp(prefix="libmpv-smoke.")
    try:
        mpv.create(
            [
                ("vo", "null"),
                ("ao", "null"),
                ("idle", "yes"),
                ("config", "no"),
                ("terminal", "no"),
                ("input-default-bindings", "no"),
                ("ytdl", "no"),
            ]
        )

        mpv_version = mpv.get_string("mpv-version")
        ffmpeg_version = mpv.get_string("ffmpeg-version")
        libplacebo_version = mpv.get_string("libplacebo-version")
        libass_version = mpv.get_node("libass-version")
        print("mpv-version: %s" % mpv_version)
        print("ffmpeg-version: %s" % ffmpeg_version)
        print("libplacebo-version: %s" % libplacebo_version)
        print("libass-version: %s" % ("0x%08x" % libass_version if isinstance(libass_version, int) else libass_version))
        print("mpv-configuration: %s" % mpv.get_string("mpv-configuration"))
        check(mpv_version == want_mpv, "mpv-version is %s, not %s" % (mpv_version, want_mpv))
        check(
            ffmpeg_version == want_ffmpeg,
            "ffmpeg-version is %s, not %s" % (ffmpeg_version, want_ffmpeg),
        )
        check(
            (libplacebo_version or "").split(" ")[0] == want_libplacebo,
            "libplacebo-version is %s, not %s" % (libplacebo_version, want_libplacebo),
        )

        decoders = {entry.get("driver") for entry in mpv.get_node("decoder-list")}
        encoders = {entry.get("driver") for entry in mpv.get_node("encoder-list")}
        protocols = set(mpv.get_node("protocol-list"))
        print(
            "%d decoders, %d encoders, %d protocols"
            % (len(decoders), len(encoders), len(protocols))
        )
        check("libdav1d" in decoders, "decoder-list has no libdav1d")
        check("https" in protocols, "protocol-list has no https")
        for encoder in ("png", "mjpeg", "libwebp"):
            check(encoder in encoders, "encoder-list has no %s" % encoder)

        # Lua, and LuaJIT in particular: the script runs on a thread of its
        # own, so its answer is waited for.
        script = os.path.join(scratch, "smoke.lua")
        with open(script, "w", encoding="utf-8") as file:
            file.write('mp.set_property("user-data/smoke/jit", jit and jit.version or "plain Lua")\n')
        jit = None
        try:
            mpv.command("load-script", script)
            deadline = time.monotonic() + 10
            while jit is None and time.monotonic() < deadline:
                for _ in mpv.events(0.2, wait=0.05):
                    pass
                jit = mpv.get_node_or_none("user-data/smoke/jit")
        except RuntimeError as error:
            check(False, "could not load a Lua script: %s" % error)
        print("Lua: %s" % jit)
        check(jit is not None and jit.startswith("LuaJIT 2.1."), "the Lua script reported %s, not LuaJIT 2.1" % jit)

        wav = os.path.join(scratch, "tone.wav")
        write_wav(wav)
        problem = play_to_end(mpv, wav, "a WAV file")
        print("WAV: %s" % (problem or "played to its end"))
        check(problem is None, problem or "")

        problem = play_to_end(mpv, "av://lavfi:sine=frequency=440:duration=0.5", "av://lavfi")
        print("av://lavfi: %s" % (problem or "played to its end"))
        check(problem is None, problem or "")
    except RuntimeError as error:
        check(False, str(error))
    finally:
        mpv.destroy()
        for name in os.listdir(scratch):
            os.remove(os.path.join(scratch, name))
        os.rmdir(scratch)

    if failures:
        print("\n%d check(s) failed for %s:" % (len(failures), path), file=sys.stderr)
        for failure in failures:
            print("  " + failure, file=sys.stderr)
        return 1
    print("\nall checks passed for %s" % path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
