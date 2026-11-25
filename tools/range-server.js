// A file server with byte ranges (Python's http.server has none), for `tvlauncher --direct <url>`: node range-server.js <dir> <port>
const http=require('http'),fs=require('fs'),path=require('path');
const dir=process.argv[2]||'/tmp/film', port=+process.argv[3]||8771;
http.createServer((req,res)=>{
  const f=path.join(dir,decodeURIComponent(req.url.split('?')[0]));
  fs.stat(f,(e,st)=>{
    if(e||!st.isFile()){res.writeHead(404);return res.end();}
    const r=req.headers.range, m=r&&/bytes=(\d+)-(\d*)/.exec(r);
    if(m){
      const a=+m[1], b=m[2]?Math.min(+m[2],st.size-1):st.size-1;
      console.log('range',a,b);
      res.writeHead(206,{'Content-Range':`bytes ${a}-${b}/${st.size}`,'Content-Length':b-a+1,'Accept-Ranges':'bytes','Content-Type':'application/octet-stream'});
      fs.createReadStream(f,{start:a,end:b}).pipe(res);
    } else {
      res.writeHead(200,{'Content-Length':st.size,'Accept-Ranges':'bytes'});
      fs.createReadStream(f).pipe(res);
    }
  });
}).listen(port,'127.0.0.1');
