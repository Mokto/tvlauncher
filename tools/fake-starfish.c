/* A stand-in for LG's libplayerAPIs and the SDL exported-window calls, for running the film player on a desktop
 * (`sh tools/film-sim.sh`). It accepts everything it is fed, answers Load with a video size, and reports what it was
 * asked to do on stderr. It decodes nothing and shows no picture. */
#include <stdio.h>
#include <string.h>

typedef void (*cb_t)(int, long long, const char *);
static cb_t g_cb;
static long long fed_v, fed_a;

void fake_ctor(void *self, const char *id) { (void)self; (void)id; fed_v = fed_a = 0; }
void fake_dtor(void *self) { (void)self; }
void fake_fg(void *self) { (void)self; }
int fake_load(void *self, const char *payload, cb_t cb) {
  (void)self;
  g_cb = cb;
  fprintf(stderr, "[fake] Load %.200s\n", payload);
  cb(4, 0, "{\"video\":{\"width\":1920,\"height\":1080}}");
  return 1;
}
int fake_loaded(void *self) { (void)self; return 1; }
int fake_play(void *self) { (void)self; fprintf(stderr, "[fake] Play (fed %lld video, %lld audio)\n", fed_v, fed_a); return 1; }
int fake_pause(void *self) { (void)self; fprintf(stderr, "[fake] Pause\n"); return 1; }
int fake_unload(void *self) {
  (void)self;
  fprintf(stderr, "[fake] Unload (fed %lld video, %lld audio)\n", fed_v, fed_a);
  if (g_cb) g_cb(23, 0, "");
  return 1;
}
void fake_feed(void **ret, void *self, const char *json) {
  (void)self;
  if (strstr(json, "\"esData\":1")) fed_v++; else fed_a++;
  ret[0] = &ret[2];
  ret[1] = (void *)2;
  memcpy(&ret[2], "Ok", 3);
}
const char *fake_create(int type) { (void)type; return "_Window_Id_1"; }
int fake_set(const char *id, void *src, void *dst) { (void)id; (void)src; (void)dst; return 0; }
void fake_destroy(const char *id) { (void)id; }

void ctor(void *, const char *) __asm__("__ZN17StarfishMediaAPIsC1EPKc");
void dtor(void *) __asm__("__ZN17StarfishMediaAPIsD1Ev");
void fg(void *) __asm__("__ZN17StarfishMediaAPIs16notifyForegroundEv");
int ld(void *, const char *, cb_t) __asm__("__ZN17StarfishMediaAPIs4LoadEPKcPFvixS1_E");
void fd(void **, void *, const char *) __asm__("__ZN17StarfishMediaAPIs4FeedB5cxx11EPKc");
int pl(void *) __asm__("__ZN17StarfishMediaAPIs4PlayEv");
int ps(void *) __asm__("__ZN17StarfishMediaAPIs5PauseEv");
int ul(void *) __asm__("__ZN17StarfishMediaAPIs6UnloadEv");
int lc(void *) __asm__("__ZN17StarfishMediaAPIs15isLoadCompletedEv");
const char *cw(int) __asm__("_SDL_webOSCreateExportedWindow");
int sw(const char *, void *, void *) __asm__("_SDL_webOSSetExportedWindow");
void dw(const char *) __asm__("_SDL_webOSDestroyExportedWindow");
void ctor(void *s, const char *i) { fake_ctor(s, i); }
void dtor(void *s) { fake_dtor(s); }
void fg(void *s) { fake_fg(s); }
int ld(void *s, const char *p, cb_t c) { return fake_load(s, p, c); }
void fd(void **r, void *s, const char *j) { fake_feed(r, s, j); }
int pl(void *s) { return fake_play(s); }
int ps(void *s) { return fake_pause(s); }
int ul(void *s) { return fake_unload(s); }
int lc(void *s) { return fake_loaded(s); }
const char *cw(int t) { return fake_create(t); }
int sw(const char *i, void *s, void *d) { return fake_set(i, s, d); }
void dw(const char *i) { fake_destroy(i); }
