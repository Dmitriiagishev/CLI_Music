"""Small, isolated adapter for the public YouTube Music catalogue."""
import json
import sys

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

def main():
    mode, value = sys.argv[1:3]
    # Filtered search in ytmusicapi 1.12.x matches English shelf names
    # ("Songs", "Artists"). Russian shelf labels are silently discarded.
    api = YTMusic(requests_session=TimedSession(), language="en")
    if mode == "search":
        limit = max(1, min(int(sys.argv[3]), 100))
        artists = api.search(value, filter="artists", limit=min(5, max(1, limit // 4)))
        songs = api.search(value, filter="songs", limit=limit)
        result = [{"type": "artist", "title": a.get("artist") or a.get("name") or "Исполнитель",
                   "url": "musicartist:" + a["browseId"]} for a in artists if a.get("browseId")]
        result.extend([x for x in (song(s) for s in songs) if x][:max(0, limit - len(result))])
        return result
    if mode == "artist":
        limit = max(1, min(int(sys.argv[3]), 100))
        items = (api.get_artist(value).get("songs") or {}).get("results") or []
        return [x for x in (song(s) for s in items[:limit]) if x]
    if mode == "lyrics":
        playlist = api.get_watch_playlist(videoId=value, limit=1)
        lyrics_id = playlist.get("lyrics")
        if not lyrics_id:
            return "Текст песни недоступен"
        data = api.get_lyrics(lyrics_id)
        if not data:
            return "Текст песни недоступен"
        lyrics = data.get("lyrics")
        if isinstance(lyrics, str):
            return lyrics or "Текст песни недоступен"
        if isinstance(lyrics, list):
            return "\n".join(line.get("text", "") for line in lyrics if isinstance(line, dict)) or "Текст песни недоступен"
        return "Текст песни недоступен"
    raise ValueError("Неизвестный запрос")

try:
    print(json.dumps(main(), ensure_ascii=False))
except Exception as error:
    print("Ошибка YouTube Music: " + str(error), file=sys.stderr)
    sys.exit(1)
