#!/usr/bin/env python3
"""Render the README gallery from curated titles and generated artwork, without real accounts."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import time

from PIL import Image, ImageDraw, ImageFont, ImageOps

REPO = Path(__file__).resolve().parents[1]
DIR = '/media/developer/tvlauncher'
OFFLINE = 'http://127.0.0.1:9'
ART = REPO / 'docs/demo-art'
COLORS = ['#586ba7', '#c35b43', '#a94475', '#31807a', '#cf844b', '#5b4491']


def tiles(name):
    with Image.open(ART / f'{name}.jpg') as atlas:
        w, h = atlas.width // 3, atlas.height // 2
        return [atlas.crop((x * w, y * h, (x + 1) * w, (y + 1) * h)) for y in range(2) for x in range(3)]


def populate(root, regular, bold):
    data = root / DIR.lstrip('/')
    cache = data / 'cache'
    cache.mkdir(parents=True)
    (root / 'luna').mkdir()
    fonts = root / 'usr/share/fonts'
    fonts.mkdir(parents=True)
    (fonts / 'LG_Smart_UI-Regular.ttf').symlink_to(regular)
    (fonts / 'LG_Smart_UI-SemiBold.ttf').symlink_to(bold)

    def write(path, value):
        path.write_text(json.dumps(value), encoding='utf-8')

    def save(im, name):
        im.save(cache / name, format='JPEG', quality=90)
        return f'{DIR}/cache/{name}'

    movies = [
        ('Arrival', '2016', 116, 'Sci-Fi', 'A linguist tries to communicate with visitors after spacecraft appear around the world.'),
        ('Blade Runner 2049', '2017', 164, 'Sci-Fi', 'A young blade runner makes a discovery that sends him looking for a missing predecessor.'),
        ('Dune', '2021', 155, 'Adventure', 'The heir to House Atreides is drawn into a struggle for control of a desert planet.'),
        ('Interstellar', '2014', 169, 'Sci-Fi', 'A crew travels through a wormhole in search of a new home for humanity.'),
        ('The Grand Budapest Hotel', '2014', 99, 'Comedy', 'A celebrated concierge and a lobby boy are caught up in the theft of a priceless painting.'),
        ('The Martian', '2015', 144, 'Sci-Fi', 'An astronaut left behind on Mars must find a way to survive while Earth plans a rescue.'),
    ]
    shows = [
        ('Andor', '2022–2025', 0, 'Sci-Fi', 'Cassian Andor is drawn into the growing rebellion against the Empire.'),
        ('Breaking Bad', '2008–2013', 0, 'Drama', 'A chemistry teacher and a former student enter the drug trade in Albuquerque.'),
        ('Chernobyl', '2019', 0, 'Drama', 'Workers, scientists and officials respond to the 1986 nuclear disaster.'),
        ('Planet Earth II', '2016', 0, 'Documentary', 'Wildlife filmed in habitats from remote islands to the heart of cities.'),
        ('Severance', '2022–', 0, 'Thriller', 'Office workers agree to split their memories between their jobs and their lives outside.'),
        ('The Bear', '2022–', 0, 'Drama', 'A fine-dining chef comes home to run his family’s Chicago sandwich shop.'),
    ]
    resume = {
        'The Martian': (.32, ''),
        'Severance': (.61, 'S1 · E4 — The You You Are'),
        'Interstellar': (.18, ''),
        'The Bear': (.42, 'S1 · E3 — Brigade'),
    }
    font = ImageFont.truetype(str(bold), 30)
    fade = Image.new('L', (1, 240))
    fade.putdata([int(235 * (y / 239) ** 1.3) for y in range(240)])
    watching = {}
    for view, entries in [('movies', movies), ('shows', shows)]:
        titles, sheet = [], tiles(view)
        for i, (name, year, minutes, genre, overview) in enumerate(entries):
            key, art = f'{view}{i}', sheet[i]
            progress, episode = resume.get(name, (0, ''))
            meta = ' · '.join(filter(None, (year, f'{minutes} min' if minutes else '', genre)))
            poster = ImageOps.fit(art, (320, 480), Image.Resampling.LANCZOS)
            poster.paste((7, 10, 16), (0, 240, 320, 480), fade.resize((320, 240)))
            ImageDraw.Draw(poster).multiline_text((20, 450), textwrap.fill(name.upper(), width=14),
                                                  font=font, fill='white', anchor='ld', spacing=4)
            save(poster, f'lib-p-{key}-demo.img')
            if name == 'The Martian':
                with Image.open(ART / 'martian.jpg') as hero:
                    bg = ImageOps.fit(hero, (1600, 900), Image.Resampling.LANCZOS)
            else:
                bg = art.resize((1600, 900), Image.Resampling.LANCZOS)
            backdrop = save(bg, f'lib-b-{key}-demo.img')
            titles.append(dict(id=key, name=name, sort=name.lower(), meta=meta, overview=overview,
                               progress=progress, genres=[genre], poster='demo', backdrop='demo'))
            if progress:
                watching[name] = dict(id=f'{key}-episode' if episode else key, title=name,
                                      sub=f'{episode or meta}\n{overview}', progress=progress, bg=backdrop,
                                      card=save(art, f'card-{key}.img'))
            if name == 'The Martian':
                write(cache / f'lib-d-{key}-page', {'item': {
                    'Id': key, 'Name': name, 'Type': 'Movie', 'Genres': [genre], 'PremiereDate': '2015-10-02',
                    'OfficialRating': 'PG-13', 'RunTimeTicks': minutes * 600_000_000, 'Overview': overview,
                    'UserData': {'PlaybackPositionTicks': int(minutes * 600_000_000 * progress)},
                    'People': [{'Name': 'Ridley Scott', 'Type': 'Director'}, {'Name': 'Matt Damon', 'Type': 'Actor'},
                               {'Name': 'Jessica Chastain', 'Type': 'Actor'}],
                    'MediaStreams': [{'Type': 'Video', 'Width': 3840, 'VideoRange': 'HDR', 'Codec': 'hevc'},
                                     {'Type': 'Audio', 'Codec': 'dts', 'Profile': 'DTS-HD MA', 'Language': 'eng', 'Channels': 8}],
                }})
        write(cache / f'lib-{view}.json', titles)
    write(cache / 'lib-views.json', [{'id': 'movies', 'name': 'Movies'}, {'id': 'shows', 'name': 'Shows'}])
    home = [watching[name] for name in resume]
    write(cache / 'watching.json', {'items': home})
    write(cache / 'backdrops.json', [{'path': it['bg'], 'title': it['title'], 'sub': it['sub']} for it in home])
    write(data / 'jellyfin.json', {'server': OFFLINE, 'serverId': 'demo', 'userId': 'demo', 'user': 'demo', 'token': 'demo'})

    apps = [('Cinema', 'C', '#493689'), ('Music', 'M', '#b33e67'), ('Arcade', 'A', '#236e7a'),
            ('Gallery', 'G', '#ad572d'), ('Weather', 'W', '#17618e'), ('Radio', 'R', '#3e7051')]
    points = []
    icon_font = ImageFont.truetype(str(bold), 90)

    def icon(label, color, name):
        im = Image.new('RGB', (192, 192), color)
        ImageDraw.Draw(im).text((96, 96), label, font=icon_font, fill='white', anchor='mm')
        return save(im, name)

    for title, glyph, color in apps:
        points.append(dict(id=f'demo.{title.lower()}', title=title, icon=icon(glyph, color, f'app-{title}.img'), bgColor=color))
    write(root / 'luna/listLaunchPoints.json', {'launchPoints': points})

    guide = [
        ('Matinee', 'Film', 'The Meridian Cut', 'Night Train', 'The Last Projection'),
        ('Wildline', 'Nature', 'Coastline', 'The Living Reef', 'Far North'),
        ('Canvas', 'Culture', 'After Hours', 'Sound & Signal', 'The Design Desk'),
        ('Dayline', 'News', 'The Daily Brief', 'The World Tonight', 'In Conversation'),
        ('Kickoff', 'Sport', 'Match Preview', 'Riverside vs. Northgate', 'Final Whistle'),
        ('Tempo', 'Music', 'Studio Sessions', 'Live at the Pavilion', 'Late Night Jazz'),
        ('Citywide', 'Local', 'City in Focus', 'Around the Corner', 'Night Market'),
        ('Latitude', 'Travel', 'Small Islands', 'A Ticket West', 'The Long Way Home'),
    ]
    now = int(time.time())
    channels, programmes = [], {}
    for i, (name, category, *shows) in enumerate(guide):
        epg = f'demo{i}'
        logo = f'{OFFLINE}/{epg}.jpg'
        # Match the FNV-1a URL key in iptv::logo_path so the simulator finds the generated logo offline.
        key = 0xcbf29ce484222325
        for byte in logo.encode():
            key = ((key ^ byte) * 0x100000001b3) % (1 << 64)
        icon(name[0], COLORS[i % len(COLORS)], f'iptv-logo-{key:016x}.img')
        channels.append(dict(name=name, group='Demo', url=f'{OFFLINE}/{epg}', epg=epg, logo=logo))
        start = now - (8 + i * 3) * 60
        programmes[epg] = [[title, start + j * 3600, start + (j + 1) * 3600, '', category,
                            'A late-night editor pieces together a missing reel before the cinema opens.' if i == j == 0 else '']
                           for j, title in enumerate(shows)]
    write(data / 'iptv.json', {'m3u': f'{OFFLINE}/demo.m3u'})
    write(data / 'state.json', {'tvFavs': ['tv:demo0', 'tv:demo2']})
    write(cache / 'iptv-channels.json', channels)
    write(cache / 'iptv-guide.json', programmes)
    return data


def render(root, binary, name, *args, mode='--shot'):
    bmp = root / f'{name}.bmp'
    subprocess.run([str(binary), mode, str(bmp), *args], check=True, env={**os.environ, 'LAUNCHER_ROOT': str(root)})
    with Image.open(bmp) as shot:
        if shot.size != (1920, 1080) or not shot.convert('RGB').getbbox():
            raise ValueError(f'{name}: expected a nonblank 1920×1080 screenshot, got {shot.size}')
        shot.convert('RGB').resize((1600, 900), Image.Resampling.LANCZOS).save(root / f'{name}.png', optimize=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    fonts = Path('/System/Library/Fonts/Supplemental')
    parser.add_argument('--regular', type=Path, default=fonts / 'Arial.ttf')
    parser.add_argument('--bold', type=Path, default=fonts / 'Arial Bold.ttf')
    args = parser.parse_args()
    binary = REPO / 'target/release/tvlauncher'
    for path in (binary, args.regular, args.bold):
        if not path.is_file():
            parser.error(f'not found: {path}')
    with tempfile.TemporaryDirectory(prefix='tvlauncher-demo-') as tmp:
        root = Path(tmp)
        data = populate(root, args.regular.resolve(), args.bold.resolve())
        shots = {'home': '', 'movies': 'URD', 'shows': 'URRDRRRR', 'title': 'URDRRRRRO',
                 'live-tv': 'URRRD', 'settings': 'URRRROLD'}
        for name, keys in shots.items():
            render(root, binary, name, keys)
        render(root, binary, 'player', 'seek', data / 'cache/lib-b-movies5-demo.img', mode='--film-shot')
        (data / 'profiles.json').write_text(json.dumps([
            dict(name=name, avatar=avatar, role=role, pin=pin, apps=[])
            for name, avatar, role, pin in [('Maya', 5, 'Manager', ''), ('Alex', 8, 'Adult', ''), ('Sam', 11, 'Kid', '1234')]
        ]), encoding='utf-8')
        render(root, binary, 'profiles', '')
        for name in [*shots, 'profiles', 'player']:
            (REPO / 'docs' / f'{name}.png').write_bytes((root / f'{name}.png').read_bytes())


if __name__ == '__main__':
    main()
