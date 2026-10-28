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
        if kwargs.get("language") != "en":
            raise AssertionError("Filtered ytmusicapi search requires English shelf names")

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

    def test_library_drops_russian_filtered_shelves(self):
        from ytmusicapi import YTMusic
        response = {"contents": {"sectionListRenderer": {"contents": [{
            "musicShelfRenderer": {
                "title": {"runs": [{"text": "Песни"}]},
                "contents": [{"musicResponsiveListItemRenderer": {}}],
            }
        }]}}}
        api = YTMusic(language="en")
        with patch.object(api, "_send_request", return_value=response), \
             patch("ytmusicapi.mixins.search.parse_search_results", return_value=[{"videoId": "abc"}]), \
             patch("ytmusicapi.mixins.search.get_continuations", return_value=[]):
            self.assertEqual(api.search("test", filter="songs"), [])
            response["contents"]["sectionListRenderer"]["contents"][0]["musicShelfRenderer"]["title"]["runs"][0]["text"] = "Songs"
            self.assertEqual(api.search("test", filter="songs"), [{"videoId": "abc"}])

    def test_artist_tracks_and_lyrics(self):
        self.assertEqual(self.call("artist", "UC123")[0]["title"], "Artist — Song")
        self.assertEqual(self.call("lyrics", "abc"), "First line\nSecond line")


if __name__ == "__main__":
    unittest.main()
