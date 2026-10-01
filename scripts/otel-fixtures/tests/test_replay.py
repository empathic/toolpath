import json
import socket
import unittest
import urllib.error
import urllib.request

import capture
from replay import ReplayServer


def post(url, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


class ReplayTest(unittest.TestCase):
    def test_serves_in_order_and_records_requests(self):
        with ReplayServer(18499, [(r"/v1/x", [{"n": 1}, {"n": 2}])]) as s:
            self.assertEqual(post(s.base_url + "/v1/x", {"a": 1}), {"n": 1})
            self.assertEqual(post(s.base_url + "/v1/x?key=k", {"a": 2}), {"n": 2})
            self.assertEqual([r["body"] for r in s.requests], [{"a": 1}, {"a": 2}])
            self.assertEqual(s.unserved(), {})
            with self.assertRaises(urllib.error.HTTPError) as err:
                post(s.base_url + "/v1/x", {})
            self.assertEqual(err.exception.code, 500)
            with self.assertRaises(urllib.error.HTTPError) as err:
                post(s.base_url + "/v1/other", {})
            self.assertEqual(err.exception.code, 404)

    def test_unserved_reports_leftovers(self):
        with ReplayServer(18498, [(r"/v1/x", [{"n": 1}])]) as s:
            self.assertEqual(s.unserved(), {"/v1/x": 1})

    def test_network_guard_refuses_non_loopback(self):
        original = socket.socket.connect
        try:
            capture.guard_network()
            with socket.socket() as sock, self.assertRaises(RuntimeError):
                sock.connect(("10.255.255.1", 443))
        finally:
            socket.socket.connect = original


if __name__ == "__main__":
    unittest.main()
