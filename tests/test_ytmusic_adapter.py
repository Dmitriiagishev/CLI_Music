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
    watch_calls = []
    def __init__(self, **kwargs):
        if kwargs.get("language") != "en":
            raise AssertionError("Filtered ytmusicapi search requires English shelf names")

    def search(self, query, filter, limit):
        if filter == "artists":
            return [{"artist": "Artist", "browseId": "UC123"}]
        if query == "many":
            return [{"title": f"Song {i}", "videoId": f"id{i}"} for i in range(min(limit, 150))]
        return [{"title": "Song", "videoId": "abc", "artists": [{"name": "Artist"}]}]

    def get_artist(self, value):
        return {"songs": {"browseId": "VLPL123", "results": [{"title": "Preview", "videoId": "old"}]}}

    def get_playlist(self, value, limit):
        if value != "PL123":
            raise AssertionError("Artist playlist ID must omit VL prefix")
        return {"tracks": [{"title": "Song" if i == 0 else f"Song {i}", "videoId": "abc" if i == 0 else f"id{i}", "artists": [{"name": "Artist"}]} for i in range(min(limit, 150))]}

    def get_watch_playlist(self, **kwargs):
        self.watch_calls.append(kwargs)
        tracks = [{"title": "Seed", "videoId": kwargs["videoId"]}, {"title": "Mix", "videoId": "mix", "artists": [{"name": "Other"}]}]
        return {"lyrics": "LYRICS_ID", "related": "RELATED_ID", "tracks": tracks}

    def get_song_related(self, value):
        assert value == "RELATED_ID"
        return [{"title": "Songs", "contents": [
            {"title": "Seed", "videoId": "abc"},
            {"title": "Related", "videoId": "rel", "artists": [{"name": "Other"}]},
            {"title": "Repeated", "videoId": "rel"},
            {"title": "Playlist", "playlistId": "PL1"},
        ]}, {"title": "About", "contents": "A description"}]

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
        many = self.call("search", "many", "120")
        self.assertEqual(len(many), 121)
        self.assertEqual(many[-1]["title"], "Song 119")

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
        tracks = self.call("artist", "UC123", "120")
        self.assertEqual(len(tracks), 120)
        self.assertEqual(tracks[-1]["title"], "Artist — Song 119")
        self.assertEqual(self.call("lyrics", "abc"), "First line\nSecond line")

    def test_details_and_radio_return_songs_without_seed_or_duplicates(self):
        details = self.call("details", "abc")
        self.assertEqual(details["lyrics"], "First line\nSecond line")
        self.assertEqual([track["url"] for track in details["related"]], ["https://music.youtube.com/watch?v=rel"])
        self.assertEqual(details["related"][0]["title"], "Other — Related")
        radio = self.call("radio", "abc")
        self.assertTrue(FakeMusic.watch_calls[-1]["radio"])
        self.assertEqual([track["url"] for track in radio], ["https://music.youtube.com/watch?v=mix"])


if __name__ == "__main__":
    unittest.main()
