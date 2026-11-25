#!/usr/bin/env python3
"""Helpers for tools/tv-test.sh (standard library only).

  tv-catalog.py item <ffprobe.json> <out.item.json>   a fake-Jellyfin item for a test file
  tv-catalog.py catalog <server> <user id> <token>    the real library sorted by what each title tests

The catalog lists, for each feature the film player has code for (Dolby Vision 5/7/8, HDR10+, TrueHD, DTS, PGS, ASS...),
the smallest title in the library that has it, so a test streams little.
"""
import json
import subprocess
import sys
import urllib.request


def item(probe):
    streams, n = [], 0
    for s in probe["streams"]:
        kind = {"video": "Video", "audio": "Audio", "subtitle": "Subtitle"}.get(s["codec_type"])
        if not kind:
            continue
        tags = s.get("tags", {})
        o = {"Type": kind, "Index": s["index"], "Codec": s["codec_name"], "Language": tags.get("language", "")}
        if kind != "Video":
            o["DisplayTitle"] = f'{tags.get("language", "und")} - {s["codec_name"].upper()}'
            o["IsForced"] = bool(s.get("disposition", {}).get("forced"))
            o["IsDefault"] = bool(s.get("disposition", {}).get("default"))
        if kind == "Subtitle":
            o["IsTextSubtitleStream"] = s["codec_name"] in ("subrip", "ass", "ssa", "webvtt", "mov_text")
        streams.append(o)
        n += 1
    ticks = int(float(probe["format"]["duration"]) * 10_000_000)
    return {
        "RunTimeTicks": ticks,
        "Chapters": [{"Name": f"Part {i + 1}", "StartPositionTicks": ticks * i // 4} for i in range(4)],
        "Trickplay": {},
        "MediaSources": [{"Id": "src1", "DefaultAudioStreamIndex": next((s["Index"] for s in streams if s["Type"] == "Audio"), -1),
                          "DefaultSubtitleStreamIndex": -1, "MediaStreams": streams}],
    }


FEATURES = [
    ("dv-p7", lambda v, a, s, c: v.get("VideoRangeType") in ("DOVIWithEL", "DOVIWithELHDR10Plus") or (v.get("DvProfile") == 7)),
    ("dv-p8", lambda v, a, s, c: v.get("DvProfile") == 8),
    ("dv-p5", lambda v, a, s, c: v.get("DvProfile") == 5 or v.get("VideoRangeType") == "DOVI"),
    ("hdr10plus", lambda v, a, s, c: "HDR10Plus" in str(v.get("VideoRangeType"))),
    ("hdr10", lambda v, a, s, c: v.get("VideoRangeType") == "HDR10"),
    ("hlg", lambda v, a, s, c: v.get("VideoRangeType") == "HLG"),
    ("h264-8bit", lambda v, a, s, c: v.get("Codec") == "h264" and (v.get("BitDepth") or 8) == 8),
    ("h264-10bit", lambda v, a, s, c: v.get("Codec") == "h264" and (v.get("BitDepth") or 8) > 8),
    ("hevc-sdr", lambda v, a, s, c: v.get("Codec") == "hevc" and v.get("VideoRangeType") == "SDR"),
    ("av1", lambda v, a, s, c: v.get("Codec") == "av1"),
    ("vp9", lambda v, a, s, c: v.get("Codec") == "vp9"),
    ("truehd", lambda v, a, s, c: any(x.get("Codec") == "truehd" for x in a)),
    ("atmos", lambda v, a, s, c: any("Atmos" in str(x.get("Profile", "")) + str(x.get("DisplayTitle", "")) for x in a)),
    ("dts", lambda v, a, s, c: any(x.get("Codec") == "dts" and "MA" not in str(x.get("Profile")) for x in a)),
    ("dts-hd-ma", lambda v, a, s, c: any(x.get("Codec") == "dts" and "MA" in str(x.get("Profile")) for x in a)),
    ("eac3", lambda v, a, s, c: any(x.get("Codec") == "eac3" for x in a)),
    ("flac", lambda v, a, s, c: any(x.get("Codec") == "flac" for x in a)),
    ("opus", lambda v, a, s, c: any(x.get("Codec") == "opus" for x in a)),
    ("pgs", lambda v, a, s, c: any(x.get("Codec") == "hdmv_pgs_subtitle" for x in s)),
    ("vobsub", lambda v, a, s, c: any(x.get("Codec") == "dvd_subtitle" for x in s)),
    ("ass", lambda v, a, s, c: any(x.get("Codec") in ("ass", "ssa") for x in s)),
    ("srt", lambda v, a, s, c: any(x.get("Codec") in ("subrip", "srt") for x in s)),
    ("forced-sub", lambda v, a, s, c: any(x.get("IsForced") for x in s)),
    ("many-audio", lambda v, a, s, c: len(a) >= 4),
    ("chapters", lambda v, a, s, c: (c or 0) >= 5),
    ("mp4", lambda v, a, s, c: c == "mp4" or c == "m4v"),
]


def classify(it):
    """The features of one library item (a Jellyfin item with MediaSources and MediaStreams), or None."""
    srcs = it.get("MediaSources") or []
    if not srcs:
        return None
    st = srcs[0].get("MediaStreams") or []
    v = next((x for x in st if x.get("Type") == "Video"), {})
    a = [x for x in st if x.get("Type") == "Audio"]
    s = [x for x in st if x.get("Type") == "Subtitle"]
    chapters = len(it.get("Chapters") or [])
    cont = (srcs[0].get("Container") or "").split(",")[0]
    out = []
    for name, f in FEATURES:
        # `c` is the container for mp4 and the chapter count for chapters
        arg = cont if name == "mp4" else chapters
        if f(v, a, s, arg):
            out.append(name)
    return out


def catalog(server, uid, token):
    url = (f"{server}/Users/{uid}/Items?Recursive=true&IncludeItemTypes=Movie,Episode&EnableTotalRecordCount=false"
           "&Fields=MediaSources,Chapters&Limit=3000")
    req = urllib.request.Request(url, headers={"Authorization": f'MediaBrowser Token="{token}"'})
    items = json.load(urllib.request.urlopen(req, timeout=60))["Items"]
    best = {}
    for it in items:
        feats = classify(it)
        size = (it.get("MediaSources") or [{}])[0].get("Size") or 1 << 60
        for f in feats or []:
            if f not in best or size < best[f][0]:
                best[f] = (size, it)
    res = {}
    for name, _ in FEATURES:
        if name in best:
            size, it = best[name]
            res[name] = {"id": it["Id"], "name": it.get("Name"), "series": it.get("SeriesName"), "mb": size // 1_000_000,
                         "source": it["MediaSources"][0]["Id"]}
    return res, len(items)


def selftest():
    sample = {
        "Chapters": [{}] * 7,
        "MediaSources": [{"Id": "s", "Container": "mkv", "MediaStreams": [
            {"Type": "Video", "Codec": "hevc", "VideoRangeType": "DOVIWithEL", "DvProfile": 7, "BitDepth": 10},
            {"Type": "Audio", "Codec": "truehd", "Profile": "TrueHD + Atmos"},
            {"Type": "Audio", "Codec": "eac3"},
            {"Type": "Subtitle", "Codec": "hdmv_pgs_subtitle"},
            {"Type": "Subtitle", "Codec": "ass", "IsForced": True}]}],
    }
    got = set(classify(sample))
    assert {"dv-p7", "truehd", "atmos", "eac3", "pgs", "ass", "forced-sub", "chapters"} <= got, got
    assert not ({"dv-p8", "hdr10", "dts", "mp4", "h264-8bit"} & got), got
    probe = {"format": {"duration": "30.0"}, "streams": [
        {"index": 0, "codec_type": "video", "codec_name": "h264"},
        {"index": 1, "codec_type": "audio", "codec_name": "ac3", "tags": {"language": "eng"}, "disposition": {"default": 1}},
        {"index": 2, "codec_type": "subtitle", "codec_name": "subrip", "tags": {"language": "fre"}, "disposition": {"forced": 1}}]}
    it = item(probe)
    assert it["RunTimeTicks"] == 300_000_000
    ms = it["MediaSources"][0]["MediaStreams"]
    assert [m["Type"] for m in ms] == ["Video", "Audio", "Subtitle"] and ms[2]["IsForced"] and ms[2]["IsTextSubtitleStream"]
    print("selftest ok")


if __name__ == "__main__":
    a = sys.argv[1:]
    if a[:1] == ["item"]:
        out = item(json.loads(subprocess.check_output(["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", a[1]])
                              if not a[1].endswith(".json") else open(a[1]).read()))
        json.dump(out, open(a[2], "w"))
    elif a[:1] == ["catalog"]:
        res, n = catalog(a[1], a[2], a[3])
        print(f"{n} titles", file=sys.stderr)
        json.dump(res, sys.stdout, indent=1)
    elif a[:1] == ["selftest"]:
        selftest()
    else:
        sys.exit(__doc__)
