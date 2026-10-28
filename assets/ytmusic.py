"""Small, isolated adapter for the public YouTube Music catalogue."""
import json
import sys
from concurrent.futures import ThreadPoolExecutor

try:
    import requests
    from ytmusicapi import YTMusic
except ImportError:
    print("Установите ytmusicapi: python3 -m pip install -r requirements.txt", file=sys.stderr)
    sys.exit(2)

class TimedSession(requests.Session):
    def request(self, method, url, **kwargs):
        kwargs.setdefault("timeout", 20)
        return super().request(method, url, **kwargs)

def song(item):
    video = item.get("videoId")
    if not video:
        return None
    artists = ", ".join(a.get("name", "") for a in (item.get("artists") or []) if a.get("name"))
    title = item.get("title") or "Без названия"
    return {"type": "track", "title": (artists + " — " if artists else "") + title,
            "url": "https://music.youtube.com/watch?v=" + video}

def lyric_text(data):
    lyrics = (data or {}).get("lyrics")
    if isinstance(lyrics, str):
        return lyrics or "Текст песни недоступен"
    if isinstance(lyrics, list):
        return "\n".join(line.get("text", "") for line in lyrics if isinstance(line, dict)) or "Текст песни недоступен"
    return "Текст песни недоступен"

def main():
    mode, value = sys.argv[1:3]
    # Filtered search in ytmusicapi 1.12.x matches English shelf names
    # ("Songs", "Artists"). Russian shelf labels are silently discarded.
    if mode == "search":
        limit = max(1, int(sys.argv[3]))
        # Keep the two network requests independent; requests.Session is not
        # shared between threads.
        def search_kind(kind, count):
            client = YTMusic(requests_session=TimedSession(), language="en")
            return client.search(value, filter=kind, limit=count)
        with ThreadPoolExecutor(max_workers=3) as pool:
            artists_future = pool.submit(search_kind, "artists", 20)
            albums_future = pool.submit(search_kind, "albums", 20)
            songs_future = pool.submit(search_kind, "songs", limit)
            artists = artists_future.result()
            albums = albums_future.result()
            songs = songs_future.result()
        result = [{"type": "artist", "title": a.get("artist") or a.get("name") or "Исполнитель",
                   "url": "musicartist:" + a["browseId"]} for a in artists if a.get("browseId")]
        for album in albums:
            if not album.get("browseId"):
                continue
            names = ", ".join(a.get("name", "") for a in (album.get("artists") or []) if isinstance(a, dict) and a.get("name"))
            names = names or album.get("artist") or ""
            title = album.get("title") or "Без названия"
            result.append({"type": "album", "title": (names + " — " if names else "") + title,
                           "url": "musicalbum:" + album["browseId"]})
        result.extend([x for x in (song(s) for s in songs) if x][:limit])
        return result
    api = YTMusic(requests_session=TimedSession(), language="en")
    if mode == "artist":
        limit = max(1, int(sys.argv[3]))
        songs = api.get_artist(value).get("songs") or {}
        browse_id = songs.get("browseId")
        if browse_id:
            playlist_id = browse_id.removeprefix("VL")
            items = api.get_playlist(playlist_id, limit=limit).get("tracks") or []
        else:
            items = songs.get("results") or []
        return [x for x in (song(s) for s in items[:limit]) if x]
    if mode == "album":
        limit = max(1, int(sys.argv[3]))
        album = api.get_album(value)
        artist = album.get("artists") or []
        items = []
        for track in (album.get("tracks") or [])[:limit]:
            item = dict(track)
            if not item.get("artists"):
                item["artists"] = artist
            if entry := song(item):
                items.append(entry)
        return items
    if mode == "lyrics":
        playlist = api.get_watch_playlist(videoId=value, limit=1)
        lyrics_id = playlist.get("lyrics")
        if not lyrics_id:
            return "Текст песни недоступен"
        data = api.get_lyrics(lyrics_id)
        if not data:
            return "Текст песни недоступен"
        return lyric_text(data)
    if mode == "details":
        watch = api.get_watch_playlist(videoId=value, limit=1)
        def load_lyrics():
            if not watch.get("lyrics"):
                return "Текст песни недоступен"
            try:
                return lyric_text(YTMusic(requests_session=TimedSession(), language="en").get_lyrics(watch["lyrics"]))
            except Exception as error:
                return "Текст недоступен: " + str(error)
        def load_related():
            if not watch.get("related"):
                return []
            return YTMusic(requests_session=TimedSession(), language="en").get_song_related(watch["related"])
        with ThreadPoolExecutor(max_workers=2) as pool:
            lyrics_future = pool.submit(load_lyrics)
            related_future = pool.submit(load_related)
            lyrics = lyrics_future.result()
            try:
                sections = related_future.result()
                error = ""
            except Exception as exc:
                sections, error = [], "Похожие песни недоступны: " + str(exc)
        found, seen = [], {value}
        for section in sections:
            contents = section.get("contents")
            if not isinstance(contents, list):
                continue
            for item in contents:
                if isinstance(item, dict) and item.get("videoId") and item["videoId"] not in seen:
                    seen.add(item["videoId"])
                    if entry := song(item):
                        found.append(entry)
        return {"lyrics": lyrics, "related": found, "related_error": error}
    if mode == "radio":
        watch = api.get_watch_playlist(videoId=value, radio=True, limit=50)
        return [entry for item in (watch.get("tracks") or []) if item.get("videoId") != value and (entry := song(item))]
    raise ValueError("Неизвестный запрос")

try:
    print(json.dumps(main(), ensure_ascii=False))
except Exception as error:
    print("Ошибка YouTube Music: " + str(error), file=sys.stderr)
    sys.exit(1)
