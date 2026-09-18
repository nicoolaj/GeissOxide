//! The Arcade engine: a Pac-Man maze in fat pixels ridden by six Tron light cycles, one per
//! spectrum band. Each cycle rides at the speed of its band's energy, turns at the next junction
//! when the band jumps, leaves a fading light wall and eats the pellets it passes over; the beat
//! is a power pellet — the walls flash and every cycle reverses. Once most pellets are eaten (or
//! on the timer) a new maze is carved and the neon changes. Original design, no source port.

use rand::RngExt;

use crate::engine::{Clock, CpuEngine};
use crate::geissoxide::palette::{self, Fade};
use crate::geissoxide::raster::Canvas;
use crate::milkdrop::audio::{Audio, FFT_SIZE, log_bands};

/// Light cycles, one per log-spaced band.
const CYCLES: usize = 6;
/// Maze tiles across the frame (sets the pixel size).
const TILES_ACROSS: usize = 80;
/// Tiles per second a cycle rides when its band sits at its long-term average.
const SPEED: f32 = 10.0;
/// A band turns its cycle when it jumps this much over its average, at most every `COOLDOWN` s.
const ONSET: f32 = 2.0;
const COOLDOWN: f32 = 0.15;
/// Sum of band levels below which the cycles stop (Pool's threshold, same units).
/// ponytail: absolute threshold in MilkDrop spectrum units.
const SILENCE: f32 = 2.0;
/// Fraction of the light walls kept per frame.
const DECAY: f32 = 0.96;
/// Wall brightness at rest (palette index; the bass adds up to 80), pellet brightness.
const WALL: f32 = 40.0;
const PELLET: u8 = 150;
/// Frames the walls stay white after a beat.
const FLASH_FRAMES: u32 = 3;
/// Fraction of the pellets eaten that triggers a new maze.
const EATEN: f32 = 0.9;
/// Neon tints: Tron cyan, orange, magenta, green.
const NEONS: [[f32; 3]; 4] = [
    [0.3, 0.9, 1.0],
    [1.0, 0.55, 0.1],
    [1.0, 0.3, 0.9],
    [0.4, 1.0, 0.4],
];
/// The four riding directions.
const DIRS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

/// A light cycle between tile `(tx, ty)` and the next one along `dir`.
#[derive(Clone, Copy)]
struct Cycle {
    tx: i32,
    ty: i32,
    dir: (i32, i32),
    /// Progress toward the next tile, `0..1`.
    progress: f32,
    /// An onset asked for a turn at the next junction.
    turn: bool,
}

/// A running Arcade visualizer.
pub struct Arcade {
    width: usize,
    height: usize,
    /// Tile size in pixels, maze size in tiles (both odd) and its pixel offset (centred).
    tile: usize,
    cols: usize,
    rows: usize,
    origin: (usize, usize),
    audio: Audio,
    bands: [(usize, usize); CYCLES],
    avg: [f32; CYCLES],
    since_onset: [f32; CYCLES],
    /// Tiles per second each cycle rides this frame.
    speed: [f32; CYCLES],
    /// Per tile: is it a wall; does it still hold a pellet.
    wall: Vec<bool>,
    pellet: Vec<bool>,
    floor_tiles: usize,
    eaten: usize,
    cycles: [Cycle; CYCLES],
    flash: u32,
    neon: usize,
    trail: Vec<u8>,
    idx: Vec<u8>,
    palette: Fade,
    rng: rand::rngs::ThreadRng,
    clock: Clock,
    rgba: Vec<u8>,
}

impl Arcade {
    /// Creates an engine rendering at `width`×`height` for audio at `sample_rate`, carving a new
    /// maze every `duration` seconds at the latest.
    pub fn new(width: usize, height: usize, sample_rate: u32, duration: f32) -> Self {
        let tile = (width / TILES_ACROSS).max(2);
        let odd = |n: usize| if n % 2 == 0 { n - 1 } else { n };
        let (cols, rows) = (odd((width / tile).max(3)), odd((height / tile).max(3)));
        let mut rng = rand::rng();
        let neon = rng.random_range(0..NEONS.len());
        let mut arcade = Self {
            width,
            height,
            tile,
            cols,
            rows,
            origin: ((width - cols * tile) / 2, (height - rows * tile) / 2),
            audio: Audio::new(sample_rate),
            bands: log_bands::<CYCLES>(),
            avg: [1.0; CYCLES],
            since_onset: [0.0; CYCLES],
            speed: [0.0; CYCLES],
            wall: Vec::new(),
            pellet: Vec::new(),
            floor_tiles: 0,
            eaten: 0,
            cycles: [Cycle {
                tx: 1,
                ty: 1,
                dir: DIRS[0],
                progress: 0.0,
                turn: false,
            }; CYCLES],
            flash: 0,
            neon,
            trail: vec![0; width * height],
            idx: vec![0; width * height],
            palette: Fade::new(palette::ramp(NEONS[neon])),
            rng,
            clock: Clock::new(duration),
            rgba: vec![255; width * height * 4],
        };
        arcade.carve();
        arcade
    }
}

impl CpuEngine for Arcade {
    fn frames_needed(&self) -> usize {
        FFT_SIZE
    }

    fn step(&mut self, pcm: &[f32]) -> &[u8] {
        if self.clock.tick() || self.eaten as f32 >= EATEN * self.floor_tiles as f32 {
            self.next();
        }
        self.audio.update(pcm, self.clock.fps, self.clock.frame);
        self.listen();
        if self.clock.beat(&self.audio) {
            self.flash = FLASH_FRAMES;
            for c in &mut self.cycles {
                // Same position seen from the tile ahead, riding back.
                c.tx += c.dir.0;
                c.ty += c.dir.1;
                c.dir = (-c.dir.0, -c.dir.1);
                c.progress = 1.0 - c.progress;
            }
        }
        for k in 0..CYCLES {
            self.ride(k);
        }
        self.draw();
        &self.rgba
    }

    /// New maze, new neon.
    fn next(&mut self) {
        self.clock.reset_switch();
        self.carve();
        self.neon = (self.neon + self.rng.random_range(1..NEONS.len())) % NEONS.len();
        self.palette.to(palette::ramp(NEONS[self.neon]));
    }
}

impl Arcade {
    /// Carves a fresh maze (iterative recursive backtracker on the odd tiles), refills the
    /// pellets, clears the light walls and drops every cycle on a random cell.
    fn carve(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        self.wall = vec![true; cols * rows];
        let mut stack = vec![(1usize, 1usize)];
        self.wall[cols + 1] = false;
        while let Some(&(x, y)) = stack.last() {
            let open: Vec<(usize, usize)> = DIRS
                .iter()
                .filter_map(|&(dx, dy)| {
                    let (nx, ny) = (x as i32 + 2 * dx, y as i32 + 2 * dy);
                    (nx > 0 && ny > 0 && (nx as usize) < cols && (ny as usize) < rows)
                        .then_some((nx as usize, ny as usize))
                })
                .filter(|&(nx, ny)| self.wall[ny * cols + nx])
                .collect();
            match open.get(self.rng.random_range(0..open.len().max(1))) {
                Some(&(nx, ny)) => {
                    self.wall[ny * cols + nx] = false;
                    self.wall[(y + ny) / 2 * cols + (x + nx) / 2] = false;
                    stack.push((nx, ny));
                }
                None => {
                    stack.pop();
                }
            }
        }
        self.pellet = self.wall.iter().map(|&w| !w).collect();
        self.floor_tiles = self.pellet.iter().filter(|&&p| p).count();
        self.eaten = 0;
        self.trail.fill(0);
        self.flash = 0;
        for k in 0..CYCLES {
            let (tx, ty) = (
                2 * self.rng.random_range(0..cols / 2) as i32 + 1,
                2 * self.rng.random_range(0..rows / 2) as i32 + 1,
            );
            let open = self.open_dirs(tx, ty, None);
            self.cycles[k] = Cycle {
                tx,
                ty,
                dir: open[self.rng.random_range(0..open.len())],
                progress: 0.0,
                turn: false,
            };
        }
    }

    fn is_floor(&self, tx: i32, ty: i32) -> bool {
        tx >= 0
            && ty >= 0
            && (tx as usize) < self.cols
            && (ty as usize) < self.rows
            && !self.wall[ty as usize * self.cols + tx as usize]
    }

    /// Directions leading to a floor tile from `(tx, ty)`, except `back`.
    fn open_dirs(&self, tx: i32, ty: i32, back: Option<(i32, i32)>) -> Vec<(i32, i32)> {
        DIRS.iter()
            .copied()
            .filter(|&d| Some(d) != back && self.is_floor(tx + d.0, ty + d.1))
            .collect()
    }

    /// Band energy → cycle speed, band jump → pending turn; silence stops everything.
    fn listen(&mut self) {
        let r = self.clock.rate(0.995);
        let mut total = 0.0;
        for (k, &(lo, hi)) in self.bands.iter().enumerate() {
            let imm = 0.5
                * (self.audio.freq[0][lo..hi].iter().sum::<f32>()
                    + self.audio.freq[1][lo..hi].iter().sum::<f32>())
                / (hi - lo) as f32;
            total += imm;
            let level = imm / self.avg[k].max(1e-3);
            self.avg[k] = self.avg[k] * r + imm * (1.0 - r);
            self.since_onset[k] += self.clock.dt;
            if level > ONSET && self.since_onset[k] >= COOLDOWN {
                self.since_onset[k] = 0.0;
                self.cycles[k].turn = true;
            }
            self.speed[k] = SPEED * level.min(3.0);
        }
        if total < SILENCE {
            self.speed = [0.0; CYCLES];
        }
    }

    /// Rides cycle `k` along its corridor, eating pellets and choosing a way at each tile.
    fn ride(&mut self, k: usize) {
        let mut c = self.cycles[k];
        c.progress += self.speed[k] * self.clock.dt;
        while c.progress >= 1.0 {
            c.progress -= 1.0;
            c.tx += c.dir.0;
            c.ty += c.dir.1;
            let i = c.ty as usize * self.cols + c.tx as usize;
            if std::mem::take(&mut self.pellet[i]) {
                self.eaten += 1;
            }
            let back = (-c.dir.0, -c.dir.1);
            let open = self.open_dirs(c.tx, c.ty, Some(back));
            let sides: Vec<_> = open.iter().copied().filter(|&d| d != c.dir).collect();
            c.dir = if c.turn && !sides.is_empty() {
                c.turn = false;
                sides[self.rng.random_range(0..sides.len())]
            } else if open.contains(&c.dir) {
                c.dir
            } else if open.is_empty() {
                back
            } else {
                open[self.rng.random_range(0..open.len())]
            };
        }
        self.cycles[k] = c;
    }

    /// Fades the light walls, stamps the cycles, draws maze and pellets over them and maps the
    /// palette.
    fn draw(&mut self) {
        for t in &mut self.trail {
            *t = (f32::from(*t) * DECAY) as u8;
        }
        let (tile, cols, rows) = (self.tile, self.cols, self.rows);
        let (ox, oy) = (self.origin.0 as f32, self.origin.1 as f32);
        let mut canvas = Canvas {
            buf: &mut self.trail,
            width: self.width,
            height: self.height,
        };
        for c in &self.cycles {
            let x = ox + (c.tx as f32 + c.dir.0 as f32 * c.progress) * tile as f32;
            let y = oy + (c.ty as f32 + c.dir.1 as f32 * c.progress) * tile as f32;
            block(&mut canvas, x as i32, y as i32, tile, 255);
        }

        self.idx.fill(0);
        let mut canvas = Canvas {
            buf: &mut self.idx,
            width: self.width,
            height: self.height,
        };
        let wall = if self.flash > 0 {
            self.flash -= 1;
            255
        } else {
            (WALL + 40.0 * self.audio.level[0].min(2.0)) as u8
        };
        let dot = (tile / 4).max(1);
        for ty in 0..rows {
            for tx in 0..cols {
                let (x, y) = (
                    self.origin.0 as i32 + (tx * tile) as i32,
                    self.origin.1 as i32 + (ty * tile) as i32,
                );
                let i = ty * cols + tx;
                if self.wall[i] {
                    block(&mut canvas, x, y, tile, wall);
                } else if self.pellet[i] {
                    let off = ((tile - dot) / 2) as i32;
                    block(&mut canvas, x + off, y + off, dot, PELLET);
                }
            }
        }
        for (i, t) in self.idx.iter_mut().zip(&self.trail) {
            *i = (*i).max(*t);
        }
        palette::apply(self.palette.tick(), &self.idx, &mut self.rgba);
    }
}

/// Brightens the `size`×`size` block at `(x, y)` to at least `c`.
fn block(canvas: &mut Canvas, x: i32, y: i32, size: usize, c: u8) {
    for dy in 0..size as i32 {
        for dx in 0..size as i32 {
            canvas.plot_max(x + dx, y + dy, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maze_is_connected_and_a_loud_band_drives_its_cycle() {
        let mut arcade = Arcade::new(160, 90, 44_100, 1e9);
        let (cols, rows) = (arcade.cols, arcade.rows);
        let mut seen = vec![false; cols * rows];
        let mut stack = vec![(1, 1)];
        while let Some((x, y)) = stack.pop() {
            if !arcade.is_floor(x, y)
                || std::mem::replace(&mut seen[y as usize * cols + x as usize], true)
            {
                continue;
            }
            stack.extend(DIRS.map(|d| (x + d.0, y + d.1)));
        }
        assert_eq!(
            seen.iter().filter(|&&s| s).count(),
            arcade.floor_tiles,
            "every floor tile must be reachable"
        );

        let frames =
            |f: fn(usize) -> f32| -> Vec<f32> { (0..FFT_SIZE).flat_map(|i| [f(i); 2]).collect() };
        let silence = frames(|_| 0.0);
        let start = arcade.cycles.map(|c| (c.tx, c.ty, c.progress));
        for _ in 0..30 {
            arcade.step(&silence);
        }
        assert_eq!(arcade.cycles.map(|c| (c.tx, c.ty, c.progress)), start);
        assert_eq!(arcade.eaten, 0);

        let bass = frames(|i| 0.8 * (i as f32 * 60.0 * std::f32::consts::TAU / 44_100.0).sin());
        for _ in 0..60 {
            arcade.step(&bass);
        }
        assert!(arcade.eaten > 0);
        assert!(
            arcade
                .rgba
                .chunks_exact(4)
                .any(|p| p[0] > 0 || p[1] > 0 || p[2] > 0)
        );
    }
}
