// A small Jellyfin for `sh tools/film-sim.sh` and `sh tools/tv-test.sh`: films with the user's views, byte ranges for
// the file, a subtitle track and the playback reports (printed). Item `m1` is `<dir>/film.mkv`; any other id `x` is
// `<dir>/x.mkv`. Runs on the TV's Node 8 too. Usage: node fake-media.js <media dir> <port>
const http = require('http'), fs = require('fs'), path = require('path');
const dir = process.argv[2] || '.', port = +process.argv[3] || 8780;
const fileOf = (id) => {
  if (id === 'm1') return path.join(dir, 'film.mkv');
  const mkv = path.join(dir, id + '.mkv');
  return fs.existsSync(mkv) ? mkv : path.join(dir, id + '.mp4');
};
// `<id>.item.json` (from tools/tv-catalog.py) replaces the item's length, chapters and streams.
const over = (id) => { try { return JSON.parse(fs.readFileSync(path.join(dir, id + '.item.json'), 'utf8')); } catch (e) { return {}; } };
const item = (id, name) => ({
  Id: id, Name: name, Type: 'Movie', ProductionYear: 2001, RunTimeTicks: 300000000,
  UserData: { PlaybackPositionTicks: +process.env.RESUME_TICKS || 0 },
  Chapters: [{ Name: 'Opening', StartPositionTicks: 0 }, { Name: 'The harbour', StartPositionTicks: 100000000 }, { Name: '', StartPositionTicks: 200000000 }],
  Trickplay: { src1: { 320: { Width: 320, Height: 180, TileWidth: 10, TileHeight: 3, ThumbnailCount: 30, Interval: 1000, Bandwidth: 1 } } },
  MediaSources: [{ Id: 'src1', DefaultAudioStreamIndex: 1, DefaultSubtitleStreamIndex: -1, MediaStreams: [
    { Type: 'Video', Index: 0 },
    { Type: 'Audio', Index: 1, DisplayTitle: 'English - AC3' },
    { Type: 'Audio', Index: 2, DisplayTitle: 'French - FLAC' },
    { Type: 'Subtitle', Index: 3, DisplayTitle: 'French - SUBRIP', IsTextSubtitleStream: true },
    { Type: 'Subtitle', Index: 4, DisplayTitle: 'English - ASS', IsTextSubtitleStream: true },
    { Type: 'Subtitle', Index: 5, DisplayTitle: 'English - Sidecar', IsTextSubtitleStream: true, IsExternal: true },
  ] }],
});
http.createServer((req, res) => {
  const u = req.url.split('?')[0];
  const json = (o) => { const b = JSON.stringify(o); res.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(b) }); res.end(b); };
  let m;
  if ((m = /^\/Users\/u1\/Items\/(\w+)$/.exec(u)) && fs.existsSync(fileOf(m[1]))) return json(Object.assign(item(m[1], m[1] === 'm1' ? 'The Test Film' : 'Test ' + m[1]), over(m[1])));
  if (/^\/MediaSegments\/\w+$/.test(u)) return json({ Items: [{ Type: 'Intro', StartTicks: 20000000, EndTicks: 80000000 }, { Type: 'Outro', StartTicks: 270000000, EndTicks: 300000000 }] });
  if (/^\/Videos\/\w+\/Trickplay\/320\/0\.jpg$/.test(u)) {
    const b = fs.readFileSync(path.join(dir, 'tile.jpg')); console.log('TILE', u);
    res.writeHead(200, { 'Content-Type': 'image/jpeg', 'Content-Length': b.length }); return res.end(b);
  }
  if (u.startsWith('/Sessions/Playing')) {
    let b = ''; req.on('data', (c) => b += c); req.on('end', () => { console.log('REPORT', u, b); res.writeHead(204); res.end(); }); return;
  }
  if (u.includes('/Subtitles/5/')) {
    const b = '1\n00:00:08,000 --> 00:00:12,000\nSidecar subtitle\n';
    res.writeHead(200, { 'Content-Length': b.length }); return res.end(b);
  }
  if ((m = /^\/Videos\/(\w+)\/stream$/.exec(u)) && fs.existsSync(fileOf(m[1]))) {
    const file = fileOf(m[1]);
    const st = fs.statSync(file); const m = /bytes=(\d+)-(\d*)/.exec(req.headers.range || '');
    if (!m) { res.writeHead(200, { 'Content-Length': st.size, 'Accept-Ranges': 'bytes' }); return fs.createReadStream(file).pipe(res); }
    const a = +m[1], b = m[2] ? Math.min(+m[2], st.size - 1) : st.size - 1;
    console.log('RANGE', a, b);
    res.writeHead(206, { 'Content-Range': `bytes ${a}-${b}/${st.size}`, 'Content-Length': b - a + 1 });
    return fs.createReadStream(file, { start: a, end: b }).pipe(res);
  }
  console.log('404', req.method, req.url); res.writeHead(404); res.end();
}).listen(port, '127.0.0.1');
