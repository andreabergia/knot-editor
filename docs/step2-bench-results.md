# Step 2 Benchmark Results — wgpu + cosmic-text

Date: 2026-06-29
Backend: wgpu_cosmic (skia backend not yet implemented — task 7)
Harness: 5s duration, 1200x800 window, PresentMode::AutoNoVsync
Fixtures: bench/fixtures/*.kfx (1M-line fixture via --tile)

## rust_sample.kfx
[bench] fixture=rust_sample.kfx lines=1577 backend=wgpu_cosmic
[wgpu_cosmic] fontdb loaded 907 faces, 12 CJK candidates
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1234 notdef=0 no_entry=0 verts=6096 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1241 notdef=0 no_entry=0 verts=6054 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1255 notdef=0 no_entry=0 verts=6066 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1237 notdef=0 no_entry=0 verts=5934 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1258 notdef=0 no_entry=0 verts=6012 y_range=[0..800]
[wgpu_cosmic] rendered 43 frames, 369 glyphs cached
rust_sample.kfx	wgpu_cosmic	frames=45	p50=103.62ms	p99=121.32ms	peak_rss=136.4MiB	cpu=42.4%

## cjk.kfx
[bench] fixture=cjk.kfx lines=25 backend=wgpu_cosmic
[wgpu_cosmic] fontdb loaded 907 faces, 12 CJK candidates
[wgpu_cosmic] size=1200x800 runs=46 glyphs=102 notdef=0 no_entry=41 verts=294 y_range=[0..920]
[wgpu_cosmic] size=1200x800 runs=46 glyphs=102 notdef=0 no_entry=41 verts=294 y_range=[0..920]
[wgpu_cosmic] size=1200x800 runs=46 glyphs=102 notdef=0 no_entry=41 verts=294 y_range=[0..920]
[wgpu_cosmic] size=1200x800 runs=46 glyphs=102 notdef=0 no_entry=41 verts=294 y_range=[0..920]
[wgpu_cosmic] rendered 156 frames, 48 glyphs cached
cjk.kfx	wgpu_cosmic	frames=157	p50=29.18ms	p99=30.81ms	peak_rss=137.9MiB	cpu=26.3%

## arabic.kfx
[bench] fixture=arabic.kfx lines=11 backend=wgpu_cosmic
[wgpu_cosmic] fontdb loaded 907 faces, 12 CJK candidates
[wgpu_cosmic] size=1200x800 runs=11 glyphs=693 notdef=0 no_entry=0 verts=3522 y_range=[0..220]
[wgpu_cosmic] size=1200x800 runs=11 glyphs=693 notdef=0 no_entry=0 verts=3522 y_range=[0..220]
[wgpu_cosmic] size=1200x800 runs=11 glyphs=693 notdef=0 no_entry=0 verts=3522 y_range=[0..220]
[wgpu_cosmic] size=1200x800 runs=11 glyphs=693 notdef=0 no_entry=0 verts=3522 y_range=[0..220]
[wgpu_cosmic] rendered 38 frames, 267 glyphs cached
arabic.kfx	wgpu_cosmic	frames=39	p50=119.73ms	p99=125.05ms	peak_rss=139.0MiB	cpu=50.6%

## emoji.kfx
[bench] fixture=emoji.kfx lines=19 backend=wgpu_cosmic
[wgpu_cosmic] fontdb loaded 907 faces, 12 CJK candidates
[wgpu_cosmic] size=1200x800 runs=23 glyphs=604 notdef=0 no_entry=3 verts=2928 y_range=[0..460]
[wgpu_cosmic] size=1200x800 runs=23 glyphs=604 notdef=0 no_entry=3 verts=2928 y_range=[0..460]
[wgpu_cosmic] size=1200x800 runs=23 glyphs=604 notdef=0 no_entry=3 verts=2928 y_range=[0..460]
[wgpu_cosmic] size=1200x800 runs=23 glyphs=604 notdef=0 no_entry=3 verts=2928 y_range=[0..460]
[wgpu_cosmic] rendered 13 frames, 194 glyphs cached
emoji.kfx	wgpu_cosmic	frames=14	p50=334.24ms	p99=343.03ms	peak_rss=143.0MiB	cpu=92.7%

## minified_js.kfx
[bench] fixture=minified_js.kfx lines=2 backend=wgpu_cosmic
[wgpu_cosmic] fontdb loaded 907 faces, 12 CJK candidates
[wgpu_cosmic] size=1200x800 runs=27 glyphs=2862 notdef=0 no_entry=0 verts=16716 y_range=[0..540]
[wgpu_cosmic] size=1200x800 runs=27 glyphs=2862 notdef=0 no_entry=0 verts=16716 y_range=[0..540]
[wgpu_cosmic] size=1200x800 runs=27 glyphs=2862 notdef=0 no_entry=0 verts=16716 y_range=[0..540]
[wgpu_cosmic] size=1200x800 runs=27 glyphs=2862 notdef=0 no_entry=0 verts=16716 y_range=[0..540]
[wgpu_cosmic] rendered 51 frames, 228 glyphs cached
minified_js.kfx	wgpu_cosmic	frames=52	p50=89.69ms	p99=93.30ms	peak_rss=141.1MiB	cpu=34.9%

## rust_sample.kfx (tiled to 1M lines)
[bench] fixture=rust_sample.kfx lines=1001395 backend=wgpu_cosmic
[wgpu_cosmic] fontdb loaded 907 faces, 12 CJK candidates
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1234 notdef=0 no_entry=0 verts=6096 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1241 notdef=0 no_entry=0 verts=6054 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1255 notdef=0 no_entry=0 verts=6066 y_range=[0..800]
[wgpu_cosmic] size=1200x800 runs=40 glyphs=1237 notdef=0 no_entry=0 verts=5934 y_range=[0..800]
[wgpu_cosmic] rendered 44 frames, 369 glyphs cached
rust_sample.kfx	wgpu_cosmic	frames=45	p50=102.28ms	p99=120.27ms	peak_rss=542.2MiB	cpu=43.5%
