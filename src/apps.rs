//! Apps the boss brain writes: Rhai scripts run in a sandbox.
//!
//! An app is a script with `fn init()` and `fn frame(t, key)`. It keeps its own state on `this`
//! (a map), and can only draw on a 320x200 canvas and read time and keys. Each call has an
//! operations budget, so a runaway loop stops the app, not the machine.
//!
//! Drawing API: clear(r,g,b)  rect(x,y,w,h,r,g,b)  circle(x,y,radius,r,g,b)  pixel(x,y,r,g,b)
//!              text(x,y,"words")  text_color(r,g,b)  rand(n) -> 0..n-1
//! Values are integers or floats; colours 0-255. `t` is seconds since start, `key` is the last
//! key pressed this frame ("" if none; "enter", "backspace", "space" or the character).

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use rhai::{CallFnOptions, Dynamic, Engine, Map, Scope, AST, FLOAT, INT};

pub const CW: usize = 320;

fn put(out: &mut [crate::Px], x: i64, y: i64, p: crate::Px) {
    if x >= 0 && y >= 0 && (x as usize) < CW && (y as usize) < CH {
        out[y as usize * CW + x as usize] = p;
    }
}
pub const CH: usize = 200;

#[derive(Clone)]
enum Cmd {
    Clear(u8, u8, u8),
    Rect(i64, i64, i64, i64, u8, u8, u8),
    Circle(i64, i64, i64, u8, u8, u8),
    Text(i64, i64, String, u8, u8, u8),
}

struct Canvas {
    cmds: Vec<Cmd>,
    ink: (u8, u8, u8),
    rng: u64,
}

pub struct App {
    pub name: String,
    engine: Engine,
    ast: AST,
    this: Dynamic,
    scope: Scope<'static>,
    canvas: Rc<RefCell<Canvas>>,
}

fn c8(v: Dynamic) -> u8 {
    num(v).clamp(0.0, 255.0) as u8
}

fn num(v: Dynamic) -> f32 {
    if let Ok(i) = v.as_int() {
        i as f32
    } else if let Ok(f) = v.as_float() {
        f
    } else {
        0.0
    }
}

fn int(v: Dynamic) -> i64 {
    num(v) as i64
}

impl App {
    pub fn new(name: &str, source: &str, seed: u64) -> Result<App, String> {
        let canvas = Rc::new(RefCell::new(Canvas { cmds: Vec::new(), ink: (255, 255, 255), rng: seed | 1 }));
        let mut engine = Engine::new();
        engine.set_max_operations(200_000);
        engine.set_max_call_levels(24);
        engine.set_max_expr_depths(64, 32);
        engine.set_max_string_size(4096);
        engine.set_max_array_size(4096);
        engine.set_max_map_size(512);
        engine.on_print(|_| {});
        engine.on_debug(|_, _, _| {});

        let c = canvas.clone();
        engine.register_fn("clear", move |r: Dynamic, g: Dynamic, b: Dynamic| {
            let mut cv = c.borrow_mut();
            cv.cmds.clear();
            cv.cmds.push(Cmd::Clear(c8(r), c8(g), c8(b)));
        });
        let c = canvas.clone();
        engine.register_fn(
            "rect",
            move |x: Dynamic, y: Dynamic, w: Dynamic, h: Dynamic, r: Dynamic, g: Dynamic, b: Dynamic| {
                let mut cv = c.borrow_mut();
                if cv.cmds.len() < 20_000 {
                    cv.cmds.push(Cmd::Rect(int(x), int(y), int(w), int(h), c8(r), c8(g), c8(b)));
                }
            },
        );
        let c = canvas.clone();
        engine.register_fn("circle", move |x: Dynamic, y: Dynamic, rad: Dynamic, r: Dynamic, g: Dynamic, b: Dynamic| {
            let mut cv = c.borrow_mut();
            if cv.cmds.len() < 20_000 {
                cv.cmds.push(Cmd::Circle(int(x), int(y), int(rad).min(400), c8(r), c8(g), c8(b)));
            }
        });
        let c = canvas.clone();
        engine.register_fn("pixel", move |x: Dynamic, y: Dynamic, r: Dynamic, g: Dynamic, b: Dynamic| {
            let mut cv = c.borrow_mut();
            if cv.cmds.len() < 20_000 {
                cv.cmds.push(Cmd::Rect(int(x), int(y), 1, 1, c8(r), c8(g), c8(b)));
            }
        });
        let c = canvas.clone();
        engine.register_fn("text_color", move |r: Dynamic, g: Dynamic, b: Dynamic| {
            c.borrow_mut().ink = (c8(r), c8(g), c8(b));
        });
        let c = canvas.clone();
        engine.register_fn("text", move |x: Dynamic, y: Dynamic, s: Dynamic| {
            let mut cv = c.borrow_mut();
            let (r, g, b) = cv.ink;
            let words: String = s.to_string().chars().filter(|ch| (' '..='~').contains(ch)).take(80).collect();
            if cv.cmds.len() < 20_000 {
                cv.cmds.push(Cmd::Text(int(x), int(y), words, r, g, b));
            }
        });
        let c = canvas.clone();
        engine.register_fn("rand", move |n: Dynamic| -> INT {
            let mut cv = c.borrow_mut();
            cv.rng ^= cv.rng << 13;
            cv.rng ^= cv.rng >> 7;
            cv.rng ^= cv.rng << 17;
            let n = int(n).max(1);
            (cv.rng % n as u64) as INT
        });
        engine.register_fn("sin", |x: FLOAT| libm::sinf(x));
        engine.register_fn("cos", |x: FLOAT| libm::cosf(x));
        engine.register_fn("sqrt", |x: FLOAT| libm::sqrtf(x.max(0.0)));
        engine.register_fn("abs", |x: FLOAT| libm::fabsf(x));
        engine.register_fn("floor", |x: FLOAT| libm::floorf(x) as INT);
        engine.register_fn("width", || CW as INT);
        engine.register_fn("height", || CH as INT);

        let ast = engine.compile(source).map_err(|e| format!("compile error: {}", e))?;
        let mut app = App { name: String::from(name), engine, ast, this: Dynamic::from_map(Map::new()), scope: Scope::new(), canvas };
        if app.ast.iter_functions().any(|f| f.name == "init") {
            let opts = CallFnOptions::new().bind_this_ptr(&mut app.this).eval_ast(false);
            app.engine
                .call_fn_with_options::<Dynamic>(opts, &mut app.scope, &app.ast, "init", ())
                .map_err(|e| format!("init failed: {}", e))?;
        }
        if !app.ast.iter_functions().any(|f| f.name == "frame") {
            return Err(String::from("the app has no fn frame(t, key)"));
        }
        Ok(app)
    }

    /// Run one frame of the app.
    pub fn frame(&mut self, t: f32, key: &str) -> Result<(), String> {
        self.canvas.borrow_mut().cmds.clear();
        let opts = CallFnOptions::new().bind_this_ptr(&mut self.this).eval_ast(false);
        self.engine
            .call_fn_with_options::<Dynamic>(opts, &mut self.scope, &self.ast, "frame", (t as FLOAT, String::from(key)))
            .map(|_| ())
            .map_err(|e: Box<rhai::EvalAltResult>| format!("{}", e))
    }

    /// Paint this frame's drawing onto a CW x CH canvas of pixels.
    pub fn paint(&self, out: &mut [crate::Px]) {
        let cv = self.canvas.borrow();
        if !matches!(cv.cmds.first(), Some(Cmd::Clear(..))) {
            for p in out.iter_mut() {
                *p = crate::Px::new(0, 0, 0);
            }
        }
        for cmd in &cv.cmds {
            match cmd {
                Cmd::Clear(r, g, b) => {
                    let p = crate::Px::new(*r, *g, *b);
                    for q in out.iter_mut() {
                        *q = p;
                    }
                }
                Cmd::Rect(x, y, w, h, r, g, b) => {
                    let p = crate::Px::new(*r, *g, *b);
                    for yy in (*y).max(0)..(*y + *h).min(CH as i64) {
                        for xx in (*x).max(0)..(*x + *w).min(CW as i64) {
                            put(out, xx, yy, p);
                        }
                    }
                }
                Cmd::Circle(cx, cy, rad, r, g, b) => {
                    let p = crate::Px::new(*r, *g, *b);
                    for yy in (cy - rad).max(0)..(cy + rad + 1).min(CH as i64) {
                        for xx in (cx - rad).max(0)..(cx + rad + 1).min(CW as i64) {
                            if (xx - cx) * (xx - cx) + (yy - cy) * (yy - cy) <= rad * rad {
                                put(out, xx, yy, p);
                            }
                        }
                    }
                }
                Cmd::Text(x, y, s, r, g, b) => {
                    let p = crate::Px::new(*r, *g, *b);
                    for (i, ch) in s.chars().enumerate() {
                        let glyph = font8x8::legacy::BASIC_LEGACY[(ch as usize).min(127)];
                        for (gy, bits) in glyph.iter().enumerate() {
                            for gx in 0..8 {
                                if bits & (1 << gx) != 0 {
                                    put(out, x + (i * 8 + gx) as i64, y + gy as i64, p);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
