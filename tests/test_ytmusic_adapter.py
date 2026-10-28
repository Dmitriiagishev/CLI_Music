import contextlib
import io
import json
import pathlib
import runpy
import sys
import unittest
from unittest.mock import patch

ADAPTER = pathlib.Path(__file__).resolve().parents[1] / "assets" / "ytmusic.py"


class FakeMusic:
    def __init__(self, **kwargs):
        pass

    def search(self, query, filter, limit):
        if filter == "artists":
            return [{"artist": "Artist", "browseId": "UC123"}]
        return [{"title": "Song", "videoId": "abc", "artists": [{"name": "Artist"}]}]

    def get_artist(self, value):
        return {"songs": {"results": [{"title": "Song", "videoId": "abc", "artists": [{"name": "Artist"}]}]}}

    def get_watch_playlist(self, **kwargs):
        return {"lyrics": "LYRICS_ID"}

    def get_lyrics(self, value):
        return {"lyrics": "First line\nSecond line"}


class AdapterTests(unittest.TestCase):
    def call(self, mode, value, limit="10"):
        output = io.StringIO()
        with patch("ytmusicapi.YTMusic", FakeMusic), patch.object(sys, "argv", [str(ADAPTER), mode, value, limit]), contextlib.redirect_stdout(output):
            runpy.run_path(str(ADAPTER), run_name="__main__")
        return json.loads(output.getvalue())

    def test_search_groups_music_artists_and_songs(self):
        result = self.call("search", "Artist song")
        self.assertEqual([entry["type"] for entry in result], ["artist", "track"])
        self.assertEqual(result[0]["url"], "musicartist:UC123")
        self.assertEqual(result[1]["url"], "https://music.youtube.com/watch?v=abc")

    def test_artist_tracks_and_lyrics(self):
        self.assertEqual(self.call("artist", "UC123")[0]["title"], "Artist — Song")
        self.assertEqual(self.call("lyrics", "abc"), "First line\nSecond line")


if __name__ == "__main__":
    unittest.main()
