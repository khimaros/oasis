"""end-to-end tests: drive the host build of the portal over real sockets."""

import hashlib
import http.client
import json
import os
import pathlib
import re
import socket
import struct
import subprocess
import tempfile
import time
import typing
import unittest
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parents[2]
BINARY = os.environ.get("OASIS_HOST_BIN", ROOT / "target" / "debug" / "oasis-host")
ADMIN_TOKEN = "sesame"
ALIAS = "oasis.local"
# loopback addresses standing in for separate client devices
LOCAL, ADA, BOB = "127.0.0.1", "127.0.0.2", "127.0.0.3"
TOPICS = ["general", "events", "marketplace", "lost", "intros"]
MAX_PINS = 8
MAX_EVENTS = 64
DNS_IP = "192.168.71.1"
STARTUP_SECS = 5
CLIENT_TIME = 1_800_000_000
CHAT_CAPACITY = 50
MAX_USERS = 100
MAX_MAILBOXES = 100
# length, timestamp, reference, name length
RECORD_HEAD = struct.Struct("<HIIB")
MAILBOX_BYTES = 8000
PASSWORD = "hunter22"
PBKDF2_ROUNDS = 1000
TYPE_A, TYPE_AAAA = 1, 28


def free_port(kind):
    with socket.socket(socket.AF_INET, kind) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def dns_query(name, qtype, query_id=0x1234):
    labels = b"".join(bytes([len(part)]) + part.encode() for part in name.split("."))
    return (
        struct.pack(">HHHHHH", query_id, 0x0100, 1, 0, 0, 0) + labels + b"\0" + struct.pack(">HH", qtype, 1)
    )


def parse_records(data, first=None):
    """stored records: `[length u16][ts u32][reference u32][name length u8]
    [name][text]`, little endian, the length counting what follows it. ids
    are positions counted from `first`. when `first` is None, every record is
    preceded by its id as a u32 instead."""
    entries, offset, next_id = [], 0, first
    while offset < len(data):
        if first is None:
            (next_id,) = struct.unpack_from("<I", data, offset)
            offset += 4
        length, ts, ref, name_length = RECORD_HEAD.unpack_from(data, offset)
        body = data[offset + RECORD_HEAD.size : offset + 2 + length]
        name, text = body[:name_length].decode(), body[name_length:].decode()
        entries.append({"id": next_id, "ts": ts, "ref": ref, "name": name, "text": text})
        offset, next_id = offset + 2 + length, next_id + 1
    return entries


def parse_page(data):
    """a segment as served: the id of its first record, the id of an older
    segment or zero, the deleted and the pinned ids, then the records."""
    first, older, deleted_count = struct.unpack_from("<IIH", data)
    offset = 10
    deleted = struct.unpack_from(f"<{deleted_count}I", data, offset)
    offset += 4 * deleted_count
    (pinned_count,) = struct.unpack_from("<H", data, offset)
    pinned = struct.unpack_from(f"<{pinned_count}I", data, offset + 2)
    entries = parse_records(data[offset + 2 + 4 * pinned_count :], first)
    kept = [entry for entry in entries if entry["id"] not in deleted]
    return {"older": older or None, "pinned": list(pinned), "entries": kept}


class Server:
    """a portal process with its own data directory and ports."""

    def __init__(self, data_dir, **env):
        self.http_port = free_port(socket.SOCK_STREAM)
        self.dns_port = free_port(socket.SOCK_DGRAM)
        self.origin = f"127.0.0.1:{self.http_port}"
        self.env = {
            "OASIS_HTTP_ADDR": self.origin,
            "OASIS_DNS_ADDR": f"127.0.0.1:{self.dns_port}",
            "OASIS_DNS_IP": DNS_IP,
            "OASIS_DATA_DIR": str(data_dir),
            "OASIS_ADMIN_TOKEN": ADMIN_TOKEN,
            "OASIS_ALIASES": ALIAS,
            "OASIS_CHAT_INTERVAL_MS": "0",
            "OASIS_BOARD_INTERVAL_MS": "0",
            **env,
        }
        # session token per client address, sent along with its requests
        self.sessions = {}
        self.start()

    def start(self):
        self.process = subprocess.Popen([BINARY], env=self.env, stdout=subprocess.DEVNULL)
        deadline = time.monotonic() + STARTUP_SECS
        while time.monotonic() < deadline:
            try:
                socket.create_connection(("127.0.0.1", self.http_port), timeout=1).close()
                return
            except OSError:
                time.sleep(0.02)
        raise RuntimeError("portal did not start")

    def stop(self):
        self.process.kill()
        self.process.wait()

    def restart(self):
        self.stop()
        self.start()

    def request(self, method, path, params=None, host=None, source=LOCAL, raw=False):
        """returns (status, headers, body), the body as text unless `raw`.
        `source` is the client address, which the host build treats as a
        distinct device."""
        session = {"session": self.sessions[source]} if source in self.sessions else {}
        encoded = urllib.parse.urlencode({**session, **(params or {})})
        if method == "GET" and encoded:
            path, encoded = f"{path}?{encoded}", ""
        address = ("127.0.0.1", self.http_port)
        conn = http.client.HTTPConnection(*address, timeout=5, source_address=(source, 0))
        conn.request(method, path, body=encoded, headers={"Host": host or self.origin})
        response = conn.getresponse()
        body = response.read() if raw else response.read().decode()
        conn.close()
        return response.status, dict(response.getheaders()), body

    def get(self, path, source=LOCAL, **params):
        status, _, body = self.request("GET", path, params, source=source)
        assert status == 200, (status, body)
        return json.loads(body)

    def post(self, path, source=LOCAL, **params):
        status, _, body = self.request("POST", path, params, source=source)
        return status, json.loads(body) if status == 200 else body

    def authenticate(self, path, source, **params):
        """signs up, logs in, or changes the profile. on success the client at
        `source` keeps the session, like a browser does."""
        status, reply = self.post(path, source=source, **params)
        if status == 200:
            self.sessions[source] = reply["session"]
        return status

    def register(self, source, username, password=PASSWORD, description=""):
        params = {"username": username, "password": password, "description": description}
        return self.authenticate("/api/register", source, **params)

    def login(self, source, username, password=PASSWORD):
        return self.authenticate("/api/login", source, username=username, password=password)

    def profile(self, source, **params):
        return self.authenticate("/api/profile", source, **params)

    def dns(self, name, qtype):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.settimeout(2)
            sock.sendto(dns_query(name, qtype), ("127.0.0.1", self.dns_port))
            return sock.recv(512)

    def thread(self, subject="hello", text="first", topic="general", source=LOCAL, **params):
        """starts a thread. returns (status, reply)."""
        return self.post("/api/board", source=source, topic=topic, subject=subject, text=text, **params)

    def reply(self, thread, text, topic="general", source=LOCAL, **params):
        return self.post("/api/reply", source=source, topic=topic, thread=thread, text=text, **params)

    def binary(self, path, source=LOCAL, **params):
        status, _, body = self.request("GET", path, params, source=source, raw=True)
        assert status == 200, (status, body)
        return body

    def page(self, path, source=LOCAL, **params):
        """one segment of a stored log, parsed the way the browser does."""
        return parse_page(self.binary(path, source, **params))

    def threads(self, topic="general", at=None):
        """one page of a topic's threads, oldest first. the reference of a
        thread marks where its replies start, and the first line of its text
        is the subject."""
        page = self.page("/api/board", topic=topic, **({"segment": at} if at else {}))
        threads = []
        for entry in page["entries"]:
            subject, _, text = entry["text"].partition("\n")
            threads.append({**entry, "marker": entry["ref"], "subject": subject, "text": text})
        return {"older": page["older"], "pinned": page["pinned"], "threads": threads}

    def subjects(self, topic="general", at=None):
        """subjects of every thread of a topic, newest first, following pagination."""
        page = self.threads(topic, at)
        subjects = [thread["subject"] for thread in reversed(page["threads"])]
        return subjects + (self.subjects(topic, page["older"]) if page["older"] else [])

    def replies(self, thread, topic="general"):
        """(name, text) of every reply to a thread, oldest first, and the
        number of requests that took. the reply starts with the id to
        continue after, zero when these were the last. every record is
        preceded by its id."""
        after, found, requests = thread["marker"] - 1, [], 0
        while after is not None:
            body = self.binary("/api/thread", topic=topic, thread=thread["id"], after=after)
            (following,) = struct.unpack_from("<I", body)
            found += [(entry["name"], entry["text"]) for entry in parse_records(body[4:])]
            after, requests = following or None, requests + 1
        return found, requests


class PortalTest(unittest.TestCase):
    ENV: typing.ClassVar = {}

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.data_dir = pathlib.Path(self.tmp.name)
        self.server = Server(self.data_dir, **self.ENV)

    def tearDown(self):
        self.server.stop()
        self.tmp.cleanup()


class CaptiveTest(PortalTest):
    def test_index_is_served(self):
        status, headers, body = self.server.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertIn("text/html", headers["Content-Type"])
        self.assertIn("<title>oasis</title>", body)

    def test_probes_for_other_hosts_redirect_to_portal(self):
        probes = [
            ("connectivitycheck.gstatic.com", "/generate_204"),
            ("captive.apple.com", "/hotspot-detect.html"),
            ("www.msftconnecttest.com", "/connecttest.txt"),
            ("example.com", "/"),
        ]
        for host, path in probes:
            status, headers, _ = self.server.request("GET", path, host=host)
            self.assertEqual(status, 302, host)
            self.assertEqual(headers["Location"], f"http://{self.server.origin}/")

    def probe(self, source, host, path):
        status, _, body = self.server.request("GET", path, host=host, source=source)
        return status, body

    def test_continue_makes_the_os_believe_it_is_online(self):
        self.assertFalse(self.server.get("/api/poll", source=ADA)["released"])
        self.assertEqual(self.server.post("/api/release", source=ADA)[0], 200)
        self.assertTrue(self.server.get("/api/poll", source=ADA)["released"])
        self.assertEqual(self.probe(ADA, "connectivitycheck.gstatic.com", "/generate_204"), (204, ""))
        status, body = self.probe(ADA, "captive.apple.com", "/hotspot-detect.html")
        self.assertEqual((status, "<TITLE>Success</TITLE>" in body), (200, True))
        self.assertEqual(
            self.probe(ADA, "www.msftconnecttest.com", "/connecttest.txt"), (200, "Microsoft Connect Test")
        )
        self.assertEqual(self.probe(ADA, "detectportal.firefox.com", "/success.txt"), (200, "success\n"))

    def test_page_has_an_onboarding_flow(self):
        body = self.server.request("GET", "/")[2]
        steps = [body.index(f'id="{step}"') for step in ("intro", "welcome", "save")]
        self.assertEqual(steps, sorted(steps), "a welcome page, and a page about bookmarking")
        # the page has no done button of its own: apple's sign-in sheet is
        # released on load so that its own "done" shows right away
        self.assertIn("APPLE_SHEET", body)
        for removed in ('id="done"', 'id="leave"', 'id="next"', "window.close"):
            self.assertNotIn(removed, body)
        intro = body[steps[0] : body.index("<header")]
        for word in ("board", "chat", "mail", "files", "regular browser", "bookmark", "home screen"):
            self.assertIn(word, intro)

    def test_sign_up_is_a_view_of_the_main_page(self):
        # it has the header with its back arrow and an address of its own,
        # like the board, instead of covering the page
        body = self.server.request("GET", "/")[2]
        main = body[body.index("<main>") : body.index("</main>")]
        self.assertRegex(main, r'<section id="account">\s*<form id="join">')
        self.assertIn("username", main)
        self.assertNotIn('id="join"', body[: body.index("<header")])

    def test_navigation_goes_through_the_address(self):
        body = self.server.request("GET", "/")[2]
        for wanted in ("history.pushState", "onpopstate", "history.back()"):
            self.assertIn(wanted, body)

    def test_android_gets_an_https_link_to_escape_the_sign_in_window(self):
        body = self.server.request("GET", "/")[2]
        self.assertLess(body.index('id="welcome"'), body.index('id="escape"'))
        self.assertLess(body.index('id="escape"'), body.index('id="join"'))
        self.assertIn("continue anyway via browser", body)
        self.assertIs(self.server.get("/api/poll")["https"], False, "the host build has no tls listener")

    def test_continue_only_affects_the_device_that_asked(self):
        self.server.post("/api/release", source=ADA)
        self.assertEqual(self.probe(BOB, "connectivitycheck.gstatic.com", "/generate_204")[0], 302)
        self.assertEqual(self.probe(ADA, "example.com", "/")[0], 302, "other sites still lead to the portal")
        self.server.restart()
        self.assertEqual(self.probe(ADA, "connectivitycheck.gstatic.com", "/generate_204")[0], 302)

    def test_page_does_not_shadow_storage_builtins(self):
        # `sessionStorage.key` is a method. using it as a slot made every
        # client share one peer key
        body = self.server.request("GET", "/")[2]
        used = set(re.findall(r"(?:local|session)Storage\.(\w+)", body))
        self.assertEqual(used & {"key", "length", "clear", "getItem", "setItem", "removeItem"}, set())

    def test_empty_lists_stay_empty(self):
        body = self.server.request("GET", "/")[2]
        for placeholder in ("nothing here yet", "no threads yet", "no description yet"):
            self.assertNotIn(placeholder, body)

    def test_page_needs_no_further_requests(self):
        body = self.server.request("GET", "/")[2]
        self.assertIn('<link rel="icon" href="data:', body)
        for external in ("<script src", '<link rel="stylesheet"', "<img", "url("):
            self.assertNotIn(external, body)

    def test_alias_host_is_served_without_redirect(self):
        status, _, body = self.server.request("GET", "/", host=ALIAS)
        self.assertEqual(status, 200)
        self.assertIn("<title>oasis</title>", body)

    def test_clients_learn_the_addresses_to_show_during_onboarding(self):
        self.assertEqual(self.server.get("/api/poll")["hosts"], [ALIAS, self.server.origin])

    def test_tabs_are_board_chat_and_files(self):
        body = self.server.request("GET", "/")[2]
        self.assertEqual(re.findall(r'data-tab="(\w+)"', body), ["board", "chat", "mail", "files"])

    def test_nav_sits_below_the_content_and_survives_the_keyboard(self):
        body = self.server.request("GET", "/")[2]
        self.assertLess(body.index("</main>"), body.index("<nav"))
        self.assertIn("interactive-widget=resizes-content", body)
        self.assertIn("visualViewport", body)

    def test_page_is_topped_by_a_gradient_band_instead_of_a_title(self):
        body = self.server.request("GET", "/")[2]
        self.assertIn('<div id="band"></div>', body)
        self.assertRegex(body, r"#band \{[^}]*linear-gradient")
        self.assertNotIn("<h1", body)

    def test_page_uses_the_chosen_look(self):
        # sky blue accent, one corner radius for cards and controls alike, and
        # a dot per topic in a color of the band
        style = re.search(r"<style>(.*?)</style>", self.server.request("GET", "/")[2], re.DOTALL)[1]
        self.assertIn("--accent: #8fc3ee;", style)
        self.assertIn("--r: 0.4rem;", style)
        radii = set(re.findall(r"border-radius: ([^;]+);", style))
        self.assertEqual(radii, {"var(--r)", "50%"}, "only the dots are round")
        self.assertIn(".dot::before", style)

    def test_page_has_an_account_button_instead_of_a_name_field(self):
        body = self.server.request("GET", "/")[2]
        for wanted in (
            'id="who"',
            'id="password"',
            'id="description"',
            'id="login"',
            'id="logout"',
            '<section id="user">',
        ):
            self.assertIn(wanted, body)
        for removed in ('id="name"', 'id="pronouns"'):
            self.assertNotIn(removed, body)

    def test_page_can_be_added_to_the_home_screen_as_an_app(self):
        body = self.server.request("GET", "/")[2]
        for name in ("mobile-web-app-capable", "apple-mobile-web-app-capable", "theme-color"):
            self.assertIn(f'<meta name="{name}"', body)

    def test_unknown_path_is_not_found(self):
        self.assertEqual(self.server.request("GET", "/nope")[0], 404)

    def test_malformed_and_oversized_requests_are_rejected(self):
        status, _, _ = self.server.request("POST", "/api/chat", {"text": "x" * 20000})
        self.assertEqual(status, 400)

    def test_dns_resolves_every_name_to_the_portal(self):
        for name in ("example.com", "connectivitycheck.gstatic.com", "a.b.c.d.test"):
            reply = self.server.dns(name, TYPE_A)
            query_id, flags, questions, answers = struct.unpack(">HHHH", reply[:8])
            self.assertEqual((query_id, questions, answers), (0x1234, 1, 1))
            self.assertTrue(flags & 0x8000, "response flag")
            self.assertEqual(flags & 0x000F, 0, "rcode")
            self.assertEqual(socket.inet_ntoa(reply[-4:]), DNS_IP)

    def test_dns_gives_empty_answer_for_other_types(self):
        reply = self.server.dns("example.com", TYPE_AAAA)
        _, flags, questions, answers = struct.unpack(">HHHH", reply[:8])
        self.assertEqual((flags & 0x000F, questions, answers), (0, 1, 0))


class ChatTest(PortalTest):
    def test_messages_are_delivered_once(self):
        self.assertEqual(self.server.get("/api/poll")["chat"], [])
        self.server.register(ADA, "ada")
        status, first = self.server.post("/api/chat", source=ADA, text="hello")
        self.assertEqual(status, 200)
        self.server.post("/api/chat", source=BOB, text="second")
        chat = self.server.get("/api/poll")["chat"]
        guest = self.server.get("/api/poll", source=BOB)["name"]
        self.assertEqual([(m["name"], m["text"]) for m in chat], [("ada", "hello"), (guest, "second")])
        newer = self.server.get("/api/poll", since=first["id"])["chat"]
        self.assertEqual([m["text"] for m in newer], ["second"])

    def test_history_is_bounded(self):
        for index in range(CHAT_CAPACITY + 5):
            self.server.post("/api/chat", text=f"m{index}")
        chat = self.server.get("/api/poll")["chat"]
        self.assertEqual(len(chat), CHAT_CAPACITY)
        self.assertEqual(chat[-1]["text"], f"m{CHAT_CAPACITY + 4}")

    def test_chat_is_lost_on_restart(self):
        self.server.post("/api/chat", text="ephemeral")
        self.server.restart()
        self.assertEqual(self.server.get("/api/poll", since=99)["chat"], [])

    def test_invalid_posts_are_rejected(self):
        for params in (
            {"text": ""},
            {"text": "  \n "},
            {"text": "x" * 281},
        ):
            self.assertEqual(self.server.post("/api/chat", **params)[0], 400, params)

    def test_control_characters_are_stripped(self):
        self.server.post("/api/chat", text="one\ttwo\x1b[31m")
        self.assertEqual(self.server.get("/api/poll")["chat"][0]["text"], "onetwo[31m")

    def test_clients_cannot_pick_a_name_per_post(self):
        self.server.post("/api/chat", name="admin", text="trust me")
        self.assertTrue(self.server.get("/api/poll")["chat"][0]["name"].startswith("~"))

    def test_clock_is_learned_from_the_first_client(self):
        self.assertEqual(self.server.get("/api/poll")["time"], 0)
        self.server.post("/api/chat", text="first", now=CLIENT_TIME)
        self.server.post("/api/chat", text="second", now=CLIENT_TIME + 99999)
        chat = self.server.get("/api/poll")["chat"]
        for message in chat:
            self.assertAlmostEqual(message["ts"], CLIENT_TIME, delta=5)


class AccountTest(PortalTest):
    def poll(self, source):
        state = self.server.get("/api/poll", source=source)
        return state["name"], state["registered"]

    def test_devices_get_distinct_default_names_that_survive_restart(self):
        (ada, _), (bob, _) = self.poll(ADA), self.poll(BOB)
        self.assertRegex(ada, r"^~[a-z]+-[a-z]+$")
        self.assertNotEqual(ada, bob)
        neighbors = {self.poll(f"127.0.2.{last}")[0] for last in range(1, 21)}
        self.assertGreater(len(neighbors), 15, "adjacent addresses spread over many names")
        self.assertEqual(self.poll(ADA), (ada, False))
        self.server.restart()
        self.assertEqual(self.poll(ADA), (ada, False))

    def test_signup_logs_the_client_in_and_survives_restart(self):
        self.assertEqual(self.server.register(ADA, "ada"), 200)
        self.assertEqual(self.poll(ADA), ("ada", True))
        self.assertFalse(self.poll(BOB)[1])
        self.server.restart()
        self.assertEqual(self.poll(ADA), ("ada", True))

    def test_login_works_from_any_device_with_the_password(self):
        self.server.register(ADA, "ada")
        self.assertEqual(self.server.login(BOB, "Ada", "wrong-password"), 403)
        self.assertEqual(self.server.login(BOB, "nobody"), 403)
        self.assertFalse(self.poll(BOB)[1])
        self.assertEqual(self.server.login(BOB, "Ada"), 200)
        self.assertEqual(self.poll(BOB), ("ada", True))

    def test_logging_out_is_forgetting_the_session(self):
        self.server.register(ADA, "ada")
        del self.server.sessions[ADA]
        self.assertFalse(self.poll(ADA)[1])

    def test_sessions_cannot_be_forged(self):
        self.server.register(ADA, "ada")
        user_id, signature = self.server.sessions[ADA].split(".")
        for forged in (f"{user_id}.{'0' * len(signature)}", f"{user_id}.", user_id, "", "x.y"):
            self.assertFalse(self.server.get("/api/poll", session=forged)["registered"], forged)

    def test_passwords_are_stored_as_salted_pbkdf2(self):
        self.server.register(ADA, "ada", password="correct horse")
        self.server.register(BOB, "bob", password="correct horse")
        lines = (self.data_dir / "users").read_text().splitlines()
        rows = [line.split("\t") for line in lines]
        for _, _, salt, stored, _ in rows:
            expected = hashlib.pbkdf2_hmac("sha256", b"correct horse", salt.encode(), PBKDF2_ROUNDS)
            self.assertEqual(stored, expected.hex())
        self.assertNotEqual(rows[0][3], rows[1][3], "equal passwords get different hashes")
        self.assertNotIn("correct horse", "".join(lines))

    def test_changing_the_password_ends_other_sessions(self):
        self.server.register(ADA, "ada")
        self.server.login(BOB, "ada")
        self.assertEqual(self.server.profile(ADA, username="ada", password="new-password"), 200)
        self.assertEqual(self.poll(ADA), ("ada", True))
        self.assertFalse(self.poll(BOB)[1], "the session from the old password is void")
        self.assertEqual(self.server.login(BOB, "ada"), 403)
        self.assertEqual(self.server.login(BOB, "ada", "new-password"), 200)

    def test_usernames_are_unique_ignoring_case(self):
        self.server.register(ADA, "ada")
        self.assertEqual(self.server.register(BOB, "Ada"), 409)
        self.assertEqual(self.server.profile(ADA, username="ADA"), 200, "an account may restyle its own name")
        self.assertEqual(self.poll(ADA), ("ADA", True))

    def test_renaming_frees_the_old_username(self):
        self.server.register(ADA, "ada")
        self.assertEqual(self.server.profile(ADA, username="lovelace"), 200)
        self.assertEqual(self.server.register(BOB, "ada"), 200)
        self.assertEqual((self.poll(ADA)[0], self.poll(BOB)[0]), ("lovelace", "ada"))
        self.assertEqual(self.server.profile(BOB, username="Lovelace"), 409)

    def test_invalid_signups_are_rejected(self):
        for username in ("", "  ", "~amber-otter", "admin", "Anon", "x" * 25, "two\nlines"):
            self.assertEqual(self.server.register(ADA, username), 400, repr(username))
        for password in ("", "short", "x" * 65):
            self.assertEqual(self.server.register(ADA, "ada", password=password), 400, repr(password))
        self.assertEqual(self.server.register(ADA, "ada", description="x" * 161), 400)
        self.assertFalse(self.poll(ADA)[1])
        self.assertEqual(self.server.profile(ADA, username="ada"), 403, "profile changes need a login")

    def test_description_is_public_and_can_be_changed(self):
        self.server.register(ADA, "ada", description="gardener, beekeeper")
        self.assertEqual(self.server.get("/api/poll", source=ADA)["description"], "gardener, beekeeper")
        info = self.server.get("/api/user", source=BOB, name="ADA")
        self.assertEqual(info, {"name": "ada", "description": "gardener, beekeeper"})
        self.server.profile(ADA, username="ada", description="retired")
        self.server.restart()
        self.assertEqual(self.server.get("/api/user", name="ada")["description"], "retired")
        self.assertEqual(self.server.request("GET", "/api/user", {"name": "nobody"})[0], 404)
        self.server.post("/api/chat", source=ADA, text="hi")
        self.assertEqual(self.server.get("/api/poll")["chat"][0]["name"], "ada")

    def test_oldest_accounts_are_evicted_when_the_table_is_full(self):
        for index in range(MAX_USERS + 1):
            self.assertEqual(self.server.register(f"127.0.1.{index + 1}", f"user{index}"), 200)
        self.assertFalse(self.poll("127.0.1.1")[1])
        self.assertEqual(self.poll("127.0.1.2"), ("user1", True))
        self.assertEqual(self.server.register(ADA, "user0"), 200, "evicted name is free again")


class MailTest(PortalTest):
    def setUp(self):
        super().setUp()
        self.server.register(ADA, "ada")
        self.server.register(BOB, "bob")

    def mail(self, source, **params):
        return self.server.post("/api/mail", source=source, **params)[0]

    def mailboxes(self):
        """owners of the stored mailboxes. a mailbox is a few plain files
        named after its owner, since a directory each would cost two blocks."""
        files = list(self.data_dir.glob("mail/*"))
        self.assertTrue(all(path.is_file() for path in files))
        return {path.name.split(".")[0] for path in files}

    def inbox(self, source):
        """(counterpart, text) pairs of a mailbox, oldest first. counterparts
        are marked `<` for received and `>` for sent, which the reference of
        a mail record tells apart."""
        entries = self.server.page("/api/mail", source=source)["entries"]
        return [(("<", ">")[entry["ref"]] + entry["name"], entry["text"]) for entry in entries]

    def test_mail_is_private_to_sender_and_recipient(self):
        self.assertEqual(self.mail(ADA, to="Bob", text="psst\nsecond line"), 200)
        self.assertEqual(self.inbox(BOB), [("<ada", "psst\nsecond line")])
        self.assertEqual(self.inbox(ADA), [(">bob", "psst\nsecond line")])
        self.server.register(LOCAL, "eve")
        self.assertEqual(self.inbox(LOCAL), [])

    def test_mail_survives_restart(self):
        self.mail(ADA, to="bob", text="see you at noon")
        self.server.restart()
        self.assertEqual(self.inbox(BOB), [("<ada", "see you at noon")])

    def test_unread_count_clears_when_the_mailbox_is_read(self):
        self.mail(ADA, to="bob", text="one")
        self.mail(ADA, to="bob", text="two")
        self.assertEqual(self.server.get("/api/poll", source=BOB)["mail"], 2)
        self.assertEqual(self.server.get("/api/poll", source=ADA)["mail"], 0)
        self.inbox(BOB)
        self.assertEqual(self.server.get("/api/poll", source=BOB)["mail"], 0)

    def test_invalid_mail_is_rejected(self):
        self.assertEqual(self.mail(ADA, to="nobody", text="hi"), 404)
        self.assertEqual(self.mail(ADA, to="~nobody-here", text="hi"), 404)
        self.assertEqual(self.mail(ADA, to="bob", text=""), 400)
        self.assertEqual(self.mail(ADA, to="bob", text="x" * 1001), 400)

    def test_guests_can_send_and_receive_mail_under_their_generated_name(self):
        guest, other = "127.0.3.1", "127.0.3.2"
        name = self.server.get("/api/poll", source=guest)["name"]
        other_name = self.server.get("/api/poll", source=other)["name"]
        self.assertEqual(self.inbox(guest), [])
        self.assertFalse((self.data_dir / "mail").exists(), "reading an empty mailbox stores nothing")
        self.assertEqual(self.mail(guest, to="bob", text="hello bob"), 200)
        self.assertEqual(self.mail(BOB, to=name, text="hello guest"), 200)
        self.assertEqual(self.mail(other, to=name.upper(), text="from a neighbor"), 200)
        self.assertEqual(self.inbox(BOB), [(f"<{name}", "hello bob"), (f">{name}", "hello guest")])
        expected = [(">bob", "hello bob"), ("<bob", "hello guest"), (f"<{other_name}", "from a neighbor")]
        self.assertEqual(self.inbox(guest), expected)
        self.assertEqual(self.server.get("/api/poll", source=guest)["mail"], 0, "reading clears the count")
        self.server.restart()
        self.assertEqual(self.inbox(guest), expected)

    def test_logging_in_switches_to_the_account_mailbox(self):
        guest = "127.0.3.1"
        self.mail(guest, to="bob", text="as a guest")
        self.server.login(guest, "ada")
        self.assertEqual(self.inbox(guest), [])
        self.mail(guest, to="bob", text="as ada")
        self.assertEqual([who for who, _ in self.inbox(BOB)], [self.inbox(BOB)[0][0], "<ada"])
        self.assertTrue(self.inbox(BOB)[0][0].startswith("<~"))

    def test_the_number_of_mailboxes_is_bounded(self):
        for index in range(MAX_MAILBOXES + 5):
            self.assertEqual(self.mail(f"127.0.4.{index + 1}", to="bob", text=f"mail {index}"), 200)
        self.assertEqual(len(self.mailboxes()), MAX_MAILBOXES)
        self.assertEqual(self.inbox("127.0.4.1"), [], "the mailbox unused for longest made room")
        self.assertEqual(len(self.inbox(f"127.0.4.{MAX_MAILBOXES + 5}")), 1)
        self.assertTrue(self.inbox(BOB), "a mailbox that keeps receiving is kept")

    def test_mailbox_keeps_only_recent_mail(self):
        for index in range(40):
            self.mail(ADA, to="bob", text=f"message {index:02} {'x' * 400}")
        kept = [text[:10] for _, text in self.inbox(BOB)]
        self.assertEqual(kept[-1], "message 39")
        self.assertNotIn("message 00", kept)
        stored = sum(path.stat().st_size for path in self.data_dir.glob("mail/*"))
        self.assertLessEqual(stored, 2 * MAILBOX_BYTES, "the sender's and the recipient's mailbox")

    def test_evicted_accounts_lose_their_mail(self):
        self.mail(ADA, to="bob", text="hello")
        for index in range(MAX_USERS):
            self.server.register(f"127.0.1.{index + 1}", f"user{index}")
        self.assertEqual(self.mailboxes(), set())


class StatusTest(PortalTest):
    def stored(self):
        return {row["name"]: row for row in self.server.get("/api/status")["stored"]}

    # devices that the host build treats as being on the wifi
    IDLE = "127.0.0.9"
    ENV: typing.ClassVar = {"OASIS_STATIONS": f"{ADA},{IDLE}"}

    def test_status_tells_app_users_from_devices_that_are_only_on_the_wifi(self):
        self.server.register(ADA, "ada")
        self.server.get("/api/poll", source=ADA, key="key-a")
        guest = self.server.get("/api/poll", source=BOB, key="key-b")["name"]
        idle = self.server.get("/api/poll", source=self.IDLE)["name"]
        expected = [
            {"name": "ada", "app": True},
            {"name": guest, "app": True},
            {"name": idle, "app": False},
        ]
        self.assertEqual(self.server.get("/api/status")["online"], expected)
        self.assertRegex(idle, r"^~[a-z]+-[a-z]+$")

    def test_ids_in_the_page_are_unique(self):
        # a duplicate id made the notification button unreachable by name
        ids = re.findall(r'\bid="([^"]+)"', self.server.request("GET", "/")[2])
        self.assertEqual(sorted(ids), sorted(set(ids)))

    def test_status_counts_what_is_stored_against_its_limit(self):
        empty = self.stored()
        self.assertEqual(list(empty), ["accounts", "mailboxes", "chat messages", "threads", "replies"])
        limits = {name: row["max"] for name, row in empty.items() if not row["bytes"]}
        self.assertEqual(
            limits, {"accounts": MAX_USERS, "mailboxes": MAX_MAILBOXES, "chat messages": CHAT_CAPACITY}
        )
        self.assertTrue(all(row["count"] == 0 and row["used"] == 0 for row in empty.values()))
        self.server.register(ADA, "ada")
        self.server.post("/api/mail", source=ADA, to="ada", text="note to self")
        self.server.post("/api/chat", text="one")
        self.server.post("/api/chat", text="two")
        thread = self.server.thread("subject", "text")[1]["id"]
        self.server.thread("events are separate", "text", topic="events")
        for index in range(3):
            self.server.reply(thread, f"reply {index}")
        self.server.post("/api/delete", topic="general", log="replies", id=1, token=ADMIN_TOKEN)
        stored = self.stored()
        counts = {name: row["count"] for name, row in stored.items()}
        expected = {"accounts": 1, "mailboxes": 1, "chat messages": 2, "threads": 2, "replies": 2}
        self.assertEqual(counts, expected)
        for name in ("threads", "replies"):
            self.assertTrue(stored[name]["bytes"])
            self.assertGreater(stored[name]["used"], 0)
            self.assertGreater(stored[name]["max"], stored[name]["used"])
        # five topics of 7 thread segments and 22 reply segments, 8000 bytes each
        self.assertEqual((stored["threads"]["max"], stored["replies"]["max"]), (5 * 7 * 8000, 5 * 22 * 8000))

    def test_status_reports_space_per_partition(self):
        self.server.thread("subject", "text")
        space = self.server.get("/api/status")["space"]
        self.assertEqual([row["name"] for row in space], ["storage"])
        self.assertGreater(space[0]["used"], 0)
        self.assertGreater(space[0]["max"], space[0]["used"])

    def test_page_has_a_status_button_left_of_the_account_button(self):
        body = self.server.request("GET", "/")[2]
        header = body[body.index("<header") : body.index("</header>")]
        self.assertLess(header.index('id="info"'), header.index('id="who"'))
        self.assertLess(header.index('id="place"'), header.index('id="info"'))
        self.assertIn('<section id="status">', body)


class NotificationTest(PortalTest):
    def test_replies_are_announced_to_clients_that_ask(self):
        first = self.server.get("/api/poll")
        self.assertEqual((first["events"], first["replies"]), (0, []))
        thread = self.server.thread()[1]["id"]
        self.server.register(ADA, "ada")
        # a client tells its own replies apart by their id
        first_reply = self.server.reply(thread, "one", source=ADA)[1]["id"]
        self.server.reply(thread, "two")
        guest = self.server.get("/api/poll")["name"]
        state = self.server.get("/api/poll", events=0)
        self.assertEqual(state["events"], 2)
        expected = [
            {"id": 1, "topic": "general", "thread": thread, "reply": first_reply, "name": "ada"},
            {"id": 2, "topic": "general", "thread": thread, "reply": first_reply + 1, "name": guest},
        ]
        self.assertEqual(state["replies"], expected)
        self.assertEqual(self.server.get("/api/poll", events=1)["replies"], expected[1:])
        self.assertEqual(self.server.get("/api/poll", events=2)["replies"], [])
        self.assertEqual(self.server.get("/api/poll")["replies"], [], "a client without a cursor gets none")
        self.assertEqual(
            self.server.get("/api/poll", events=99)["replies"], expected, "cursor from before a reboot"
        )

    def test_announcements_are_bounded(self):
        thread = self.server.thread()[1]["id"]
        for index in range(MAX_EVENTS + 6):
            self.server.reply(thread, f"reply {index}")
        state = self.server.get("/api/poll", events=0)
        self.assertEqual(state["events"], MAX_EVENTS + 6)
        self.assertEqual([reply["id"] for reply in state["replies"]], list(range(7, MAX_EVENTS + 7)))

    def test_page_has_a_notification_button(self):
        body = self.server.request("GET", "/")[2]
        header = body[body.index("<header") : body.index("</header>")]
        self.assertLess(header.index('id="bell"'), header.index('id="info"'))
        self.assertIn('<section id="notes">', body)


class RateLimitTest(PortalTest):
    ENV: typing.ClassVar = {"OASIS_CHAT_INTERVAL_MS": "60000"}

    def test_rapid_posts_are_throttled(self):
        self.assertEqual(self.server.post("/api/chat", text="one")[0], 200)
        self.assertEqual(self.server.post("/api/chat", text="two")[0], 429)
        self.assertEqual(self.server.thread(text="other kinds are independent")[0], 200)


class BoardTest(PortalTest):
    # the board budget is split evenly between the topics. a quarter of a
    # topic's share holds its threads, the rest their replies
    ENV: typing.ClassVar = {"OASIS_SEGMENT_BYTES": "256", "OASIS_BOARD_BYTES": str(4096 * len(TOPICS))}
    THREAD_BYTES = 1024

    def test_topics_are_separate(self):
        for topic in TOPICS:
            self.assertEqual(self.server.thread(f"about {topic}", topic=topic)[0], 200)
        for topic in TOPICS:
            self.assertEqual(self.server.subjects(topic), [f"about {topic}"])

    def test_unknown_topics_are_rejected(self):
        for topic in ("nope", "news", "../general"):
            self.assertEqual(self.server.thread(topic=topic)[0], 404, topic)
            self.assertEqual(self.server.request("GET", "/api/board", {"topic": topic})[0], 404, topic)

    def test_page_describes_every_topic(self):
        body = self.server.request("GET", "/")[2]
        listed = re.search(r"const TOPICS = (\{.*?\});", body, re.DOTALL)[1]
        self.assertEqual(re.findall(r"^\s*(\w+): \['[^']+', '[^']+'\],$", listed, re.MULTILINE), TOPICS)
        # the back arrow sits at the top left and starts out hidden. the
        # css rule keeps `hidden` working on elements with a display style
        self.assertLess(
            body.index('id="threadlist"'), body.index('id="threadform"'), "new thread form is below"
        )
        header = body[body.index("<header") : body.index("</header>")]
        self.assertRegex(header, r'<button class="square" id="back" hidden><svg><use href="#chevron"/>')
        self.assertRegex(header, r'<button class="square" id="who"><svg><use href="#person"/>')
        self.assertLess(header.index('id="back"'), header.index('id="who"'))
        self.assertIn("[hidden] { display: none !important; }", body)

    def test_threads_survive_restart(self):
        text = 'line one\nline "two" \\n back\\slash ' + chr(0xE9) + chr(0x4E16)
        self.server.register(LOCAL, "ada")
        self.server.thread("garden swap", text, now=CLIENT_TIME)
        self.server.restart()
        thread = self.server.threads()["threads"][0]
        self.assertEqual((thread["name"], thread["subject"], thread["text"]), ("ada", "garden swap", text))
        self.assertAlmostEqual(thread["ts"], CLIENT_TIME, delta=5)
        self.assertEqual(self.server.thread("after")[1]["id"], thread["id"] + 1)

    def test_threads_need_a_subject_and_a_text(self):
        for subject, text in (
            ("", "text"),
            ("  ", "text"),
            ("x" * 81, "text"),
            ("two\nlines", "text"),
            ("ok", ""),
        ):
            self.assertEqual(self.server.thread(subject, text)[0], 400, (subject, text))
        self.assertEqual(self.server.subjects(), [])

    def test_replies_belong_to_their_thread(self):
        self.server.register(ADA, "ada")
        self.server.thread("swap seeds", "anyone?", source=ADA)
        self.server.thread("lost cat", "grey, shy")
        seeds, cat = self.server.threads()["threads"]
        self.assertEqual(self.server.reply(seeds["id"], "i have beans")[0], 200)
        self.assertEqual(self.server.reply(cat["id"], "seen near the well", source=ADA)[0], 200)
        self.assertEqual(self.server.reply(seeds["id"], "peas here\nand squash", source=ADA)[0], 200)
        guest = self.server.get("/api/poll")["name"]
        self.server.restart()
        self.assertEqual(
            self.server.replies(seeds)[0], [(guest, "i have beans"), ("ada", "peas here\nand squash")]
        )
        self.assertEqual(self.server.replies(cat)[0], [("ada", "seen near the well")])

    def test_replies_need_an_existing_thread_and_a_text(self):
        thread = self.server.thread()[1]["id"]
        self.assertEqual(self.server.reply(thread + 1, "hi")[0], 404)
        self.assertEqual(self.server.reply("x", "hi")[0], 404)
        self.assertEqual(self.server.reply(thread, "")[0], 400)
        self.assertEqual(self.server.reply(thread, "x" * 2001)[0], 400)

    def test_thread_list_pagination_reaches_older_segments(self):
        posted = [f"thread {index:02}" for index in range(12)]
        for subject in posted:
            self.server.thread(subject, "x")
        self.assertIsNotNone(self.server.threads()["older"])
        self.assertEqual(self.server.subjects(), posted[::-1])

    def test_oldest_threads_are_evicted_when_full(self):
        posted = [f"thread {index:03} {'x' * 30}" for index in range(60)]
        for subject in posted:
            self.server.thread(subject, "body")
        kept = self.server.subjects()
        self.assertEqual(kept, posted[: -len(kept) - 1 : -1])
        self.assertLess(len(kept), len(posted))
        stored = sum(path.stat().st_size for path in self.segments("general.t."))
        self.assertLessEqual(stored, self.THREAD_BYTES)
        self.server.restart()
        self.assertEqual(self.server.subjects(), kept)

    def segments(self, prefix):
        """segment files of one log. all logs of the board share a directory
        and are told apart by a prefix, since a directory each would cost
        two blocks of flash."""
        files = sorted(self.data_dir.glob(f"topics/{prefix}[0-9]*"))
        self.assertTrue(all(path.is_file() for path in self.data_dir.glob("topics/*")))
        return files

    def test_record_torn_by_power_loss_is_dropped(self):
        self.server.thread("before")
        self.server.stop()
        (segment,) = self.segments("general.t.")
        with segment.open("ab") as file:
            # a record that announces 40 bytes and stops after 8 of them
            file.write(RECORD_HEAD.pack(40, 0, 1, 3) + b"ada"[:0] + b"cut")
        self.server.start()
        second = self.server.thread("after")[1]["id"]
        self.assertEqual(self.server.subjects(), ["after", "before"])
        self.assertEqual([thread["id"] for thread in self.server.threads()["threads"]], [second - 1, second])

    def test_records_are_stored_compactly(self):
        name = self.server.get("/api/poll")["name"]
        thread = self.server.thread("s", "t")[1]["id"]
        text = ("a reply of some length, " * 4).strip()
        self.server.reply(thread, text)
        (segment,) = self.segments("general.r.")
        self.assertEqual(segment.stat().st_size, RECORD_HEAD.size + len(name) + len(text))


class LongThreadTest(PortalTest):
    def test_long_threads_are_read_in_pages(self):
        self.server.thread("busy")
        self.server.thread("quiet")
        busy, quiet = self.server.threads()["threads"]
        posted = [f"reply {index:03} {'x' * 100}" for index in range(250)]
        for index, text in enumerate(posted):
            self.server.reply(busy["id"], text)
            if index % 50 == 0:
                self.server.reply(quiet["id"], f"aside {index}")
        replies, requests = self.server.replies(busy)
        self.assertEqual([text for _, text in replies], posted)
        self.assertGreater(requests, 1, "a long thread takes several requests")
        self.assertEqual(len(self.server.replies(quiet)[0]), 5)


class AdminTest(PortalTest):
    def admin(self, path, **params):
        return self.server.post(path, topic="general", token=ADMIN_TOKEN, **params)

    def test_admin_deletes_threads_and_replies(self):
        keep = self.server.thread("keep")[1]["id"]
        spam = self.server.thread("spam")[1]["id"]
        self.assertEqual(self.server.post("/api/delete", topic="general", id=spam)[0], 403)
        self.assertEqual(self.admin("/api/delete", id=spam), (200, {"deleted": True}))
        self.assertEqual(self.admin("/api/delete", id=spam), (200, {"deleted": False}))
        self.server.reply(keep, "fine")
        rude = self.server.reply(keep, "rude")[1]["id"]
        self.assertEqual(self.admin("/api/delete", id=rude, log="replies"), (200, {"deleted": True}))
        self.server.restart()
        page = self.server.threads()
        self.assertEqual([thread["id"] for thread in page["threads"]], [keep])
        self.assertEqual([text for _, text in self.server.replies(page["threads"][0])[0]], ["fine"])
        self.assertEqual(self.server.reply(spam, "to a deleted thread")[0], 404)

    def test_admin_pins_threads(self):
        first = self.server.thread("rules")[1]["id"]
        second = self.server.thread("welcome")[1]["id"]
        self.assertEqual(self.server.post("/api/pin", topic="general", id=first, pinned=1)[0], 403)
        self.assertEqual(self.server.threads()["pinned"], [])
        self.assertEqual(self.admin("/api/pin", id=second, pinned=1)[0], 200)
        self.assertEqual(self.admin("/api/pin", id=first, pinned=1)[0], 200)
        self.assertEqual(self.admin("/api/pin", id=999, pinned=1)[0], 404)
        self.server.restart()
        self.assertEqual(self.server.threads()["pinned"], [first, second])
        self.assertEqual(self.server.threads("events")["pinned"], [], "pins are per topic")
        self.assertEqual(self.admin("/api/pin", id=second, pinned=0)[0], 200)
        self.assertEqual(self.server.threads()["pinned"], [first])

    def test_pins_are_limited(self):
        threads = [self.server.thread(f"thread {index}")[1]["id"] for index in range(MAX_PINS + 1)]
        statuses = [self.admin("/api/pin", id=thread, pinned=1)[0] for thread in threads]
        self.assertEqual(statuses, [200] * MAX_PINS + [400])


class PinnedPageTest(PortalTest):
    ENV: typing.ClassVar = {"OASIS_SEGMENT_BYTES": "256"}

    def test_a_pinned_thread_can_be_fetched_from_an_older_page(self):
        pinned = self.server.thread("rules")[1]["id"]
        self.server.post("/api/pin", topic="general", token=ADMIN_TOKEN, id=pinned, pinned=1)
        for index in range(20):
            self.server.thread(f"thread {index:02}", "x" * 40)
        newest = self.server.threads()
        self.assertEqual(newest["pinned"], [pinned])
        self.assertNotIn(pinned, [thread["id"] for thread in newest["threads"]])
        later = newest["threads"][0]["id"]
        self.assertIn("rules", [thread["subject"] for thread in self.server.threads(at=pinned)["threads"]])
        self.assertIn(later, [thread["id"] for thread in self.server.threads(at=later)["threads"]])


class NoAdminTest(PortalTest):
    def setUp(self):
        super().setUp()
        self.server.stop()
        del self.server.env["OASIS_ADMIN_TOKEN"]
        self.server.start()

    def test_admin_is_disabled_without_a_token(self):
        self.assertFalse(self.server.get("/api/poll")["admin"])
        thread = self.server.thread()[1]["id"]
        self.assertEqual(self.server.post("/api/pin", topic="general", id=thread, pinned=1, token="")[0], 403)
        self.assertEqual(self.server.post("/api/delete", topic="general", id=thread, token="")[0], 403)


class SignalTest(PortalTest):
    def test_peers_see_each_other(self):
        self.server.register(ADA, "ada")
        ada = self.server.get("/api/poll", source=ADA, key="key-a")
        bob = self.server.get("/api/poll", source=BOB, key="key-b")
        self.assertEqual(ada["peers"], [])
        self.assertEqual(bob["peers"], [{"id": ada["me"], "name": "ada"}])
        self.assertIsNone(self.server.get("/api/poll")["me"])

    def test_signals_reach_only_their_target_once(self):
        ada = self.server.get("/api/poll", key="key-a")["me"]
        bob = self.server.get("/api/poll", key="key-b")["me"]
        payload = json.dumps({"type": "offer", "sdp": "v=0\r\na=candidate x.local"})
        self.assertEqual(self.server.post("/api/signal", key="key-a", to=bob, data=payload)[0], 200)
        self.assertEqual(self.server.get("/api/poll", key="key-a")["signals"], [])
        signals = self.server.get("/api/poll", key="key-b")["signals"]
        self.assertEqual(signals, [{"from": ada, "ip": "127.0.0.1", "data": payload}])
        self.assertEqual(self.server.get("/api/poll", key="key-b")["signals"], [])

    def test_bad_signals_are_rejected(self):
        bob = self.server.get("/api/poll", key="key-b")["me"]
        self.assertEqual(self.server.post("/api/signal", key="stranger", to=bob, data="x")[0], 503)
        self.server.get("/api/poll", key="key-a")
        self.assertEqual(self.server.post("/api/signal", key="key-a", to=999, data="x")[0], 503)
        self.assertEqual(self.server.post("/api/signal", key="key-a", to=bob, data="x" * 4097)[0], 400)
        self.assertEqual(self.server.post("/api/signal", key="key-a", to=bob, data="")[0], 400)


if __name__ == "__main__":
    unittest.main()
