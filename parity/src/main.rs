//! Trade-by-trade parity check for the RSI(2) Pine -> MT5 conversion
//! (`pine/RSI2_Connors.pine` -> `mt5/RSI2_Connors.mq5`). An independent third implementation of the Pine
//! semantics re-simulates the strategy on the exact bars the EA saw, then every
//! EA trade is matched against it.
//!
//! # What "parity" means here (the client-facing definition)
//!
//! Same signals on the same bars given the same data. Pine evaluates on bar
//! close and fills at the next bar's open; RSI is ta.rsi (RMA seeded with the
//! SMA of the first `len` changes, down==0 -> 100, up==0 -> 0). Price-feed
//! differences between TradingView and a broker are a separate, explained
//! layer, not a logic defect.
//!
//! # Modes
//!
//! * `--bars F --trades F` : EA parity log (Common\Files) -> full comparison.
//! * `--ohlc F`            : any time,open,high,low,close CSV -> reference-only run
//!   (self-test before an MT5 backtest exists).

use std::collections::BTreeMap;
use std::process::ExitCode;

fn arg(a: &[String], n: &str) -> Option<String> {
    a.iter().position(|x| x == n).and_then(|i| a.get(i + 1).cloned())
}

struct Bar {
    time: String,
    live: bool, // evaluated by the EA (phase L); history-only bars are false
    open: f64,
    close: f64,
    ea: Option<(f64, f64, f64)>, // EA's rsi, trend, exit as logged
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act { Buy, Sell, CloseLong, CloseShort }

impl Act {
    fn parse(s: &str) -> Option<Act> {
        match s.trim() {
            "BUY" => Some(Act::Buy),
            "SELL" => Some(Act::Sell),
            "CLOSE_LONG" => Some(Act::CloseLong),
            "CLOSE_SHORT" => Some(Act::CloseShort),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self { Act::Buy => "BUY", Act::Sell => "SELL", Act::CloseLong => "CLOSE_LONG", Act::CloseShort => "CLOSE_SHORT" }
    }
}

struct Params { rsi_len: usize, buy_below: f64, sell_above: f64, trend_len: usize, exit_len: usize, shorts: bool }

/// Pine ta.rsi, value per bar (NaN until defined).
fn pine_rsi(c: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() <= len { return out; }
    let (mut up, mut dn) = (0.0, 0.0);
    for i in 1..=len {
        let ch = c[i] - c[i - 1];
        up += ch.max(0.0);
        dn += (-ch).max(0.0);
    }
    up /= len as f64;
    dn /= len as f64;
    let a = 1.0 / len as f64;
    let val = |u: f64, d: f64| if d == 0.0 { 100.0 } else if u == 0.0 { 0.0 } else { 100.0 - 100.0 / (1.0 + u / d) };
    out[len] = val(up, dn);
    for i in len + 1..c.len() {
        let ch = c[i] - c[i - 1];
        up = a * ch.max(0.0) + (1.0 - a) * up;
        dn = a * (-ch).max(0.0) + (1.0 - a) * dn;
        out[i] = val(up, dn);
    }
    out
}

fn sma(c: &[f64], len: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    let mut s = 0.0;
    for i in 0..c.len() {
        s += c[i];
        if i >= len { s -= c[i - len]; }
        if i + 1 >= len { out[i] = s / len as f64; }
    }
    out
}

/// Signal actions plus the reference RSI, trend SMA and exit SMA series.
type Sim = (Vec<(usize, Act)>, Vec<f64>, Vec<f64>, Vec<f64>);

/// Reference simulation: (signal bar index, action). Position starts flat at the
/// first live bar, exactly as the EA does at the start of a test.
fn simulate(bars: &[Bar], p: &Params) -> Sim {
    let c: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let (rsi, trend, exit) = (pine_rsi(&c, p.rsi_len), sma(&c, p.trend_len), sma(&c, p.exit_len));
    let mut pos = 0i32;
    let mut acts = Vec::new();
    for i in 0..bars.len() {
        if !bars[i].live || rsi[i].is_nan() || trend[i].is_nan() || exit[i].is_nan() { continue; }
        let cl = c[i];
        let act = if pos == 0 && cl > trend[i] && rsi[i] < p.buy_below {
            Some(Act::Buy)
        } else if pos == 0 && p.shorts && cl < trend[i] && rsi[i] > p.sell_above {
            Some(Act::Sell)
        } else if pos > 0 && cl > exit[i] {
            Some(Act::CloseLong)
        } else if pos < 0 && cl < exit[i] {
            Some(Act::CloseShort)
        } else {
            None
        };
        if let Some(a) = act {
            acts.push((i, a));
            pos = match a { Act::Buy => 1, Act::Sell => -1, _ => 0 };
        }
    }
    (acts, rsi, trend, exit)
}

/// Closed round trips from an action list: (P&L in price units, won?).
fn round_trips(acts: &[(usize, Act)], fill: &dyn Fn(usize) -> Option<f64>) -> Vec<(f64, bool)> {
    let mut out = Vec::new();
    let mut open: Option<(f64, f64)> = None; // (dir, price)
    for &(i, a) in acts {
        let Some(px) = fill(i) else { continue };
        match a {
            Act::Buy => open = Some((1.0, px)),
            Act::Sell => open = Some((-1.0, px)),
            Act::CloseLong | Act::CloseShort => {
                if let Some((d, e)) = open.take() {
                    let pnl = d * (px - e);
                    out.push((pnl, pnl > 0.0));
                }
            }
        }
    }
    out
}

fn summary(label: &str, rt: &[(f64, bool)], pip: f64) {
    if rt.is_empty() { println!("  {label:<28} no closed trades"); return; }
    let n = rt.len() as f64;
    let wins = rt.iter().filter(|x| x.1).count() as f64;
    let tot: f64 = rt.iter().map(|x| x.0).sum();
    let gross_win: f64 = rt.iter().filter(|x| x.0 > 0.0).map(|x| x.0).sum();
    let gross_loss: f64 = -rt.iter().filter(|x| x.0 < 0.0).map(|x| x.0).sum::<f64>();
    println!("  {label:<28} trades {:>4}  WR {:>5.1}%  total {:>+9.1} pips  avg {:>+6.2} pips  PF {:>5.2}  (lev 1 lot, no compounding)",
        rt.len(), 100.0 * wins / n, tot / pip, tot / pip / n, if gross_loss > 0.0 { gross_win / gross_loss } else { f64::INFINITY });
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let p = Params {
        rsi_len: arg(&argv, "--rsi-len").and_then(|s| s.parse().ok()).unwrap_or(2),
        buy_below: arg(&argv, "--buy-below").and_then(|s| s.parse().ok()).unwrap_or(5.0),
        sell_above: arg(&argv, "--sell-above").and_then(|s| s.parse().ok()).unwrap_or(95.0),
        trend_len: arg(&argv, "--trend-len").and_then(|s| s.parse().ok()).unwrap_or(200),
        exit_len: arg(&argv, "--exit-len").and_then(|s| s.parse().ok()).unwrap_or(5),
        shorts: !argv.iter().any(|a| a == "--no-shorts"),
    };
    let pip: f64 = arg(&argv, "--pip").and_then(|s| s.parse().ok()).unwrap_or(0.0001);

    // ---- load bars
    let (path, ea_mode) = match (arg(&argv, "--bars"), arg(&argv, "--ohlc")) {
        (Some(b), _) => (b, true),
        (None, Some(o)) => (o, false),
        _ => { eprintln!("usage: parity --bars rsi2_bars.csv --trades rsi2_trades.csv | --ohlc data.csv  [--pip 0.0001]"); return ExitCode::FAILURE; }
    };
    let text = match std::fs::read_to_string(&path) { Ok(t) => t, Err(e) => { eprintln!("{path}: {e}"); return ExitCode::FAILURE; } };
    let mut bars = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if ea_mode {
            if f.len() < 6 { continue; }
            let (Ok(o), Ok(c)) = (f[2].parse::<f64>(), f[5].parse::<f64>()) else { continue };
            let ea = if f.len() >= 9 { match (f[6].parse(), f[7].parse(), f[8].parse()) { (Ok(a), Ok(b), Ok(d)) => Some((a, b, d)), _ => None } } else { None };
            bars.push(Bar { time: f[0].to_string(), live: f[1] == "L", open: o, close: c, ea });
        } else {
            if f.len() < 5 { continue; }
            let (Ok(o), Ok(c)) = (f[1].parse::<f64>(), f[4].parse::<f64>()) else { continue };
            if o <= 0.0 || c <= 0.0 { continue; }
            bars.push(Bar { time: f[0].chars().take(16).collect(), live: true, open: o, close: c, ea: None });
        }
    }
    // --from DATE: compare only from here (e.g. where the broker's minute data starts).
    // Earlier bars still feed the indicators; both sides start flat at DATE.
    let from = arg(&argv, "--from").unwrap_or_default();
    if !from.is_empty() {
        for b in bars.iter_mut() { if b.time < from { b.live = false; } }
    }
    if bars.len() <= p.trend_len { eprintln!("only {} bars", bars.len()); return ExitCode::FAILURE; }
    let live = bars.iter().filter(|b| b.live).count();
    println!("RSI({}) Connors parity — {} bars ({} evaluated) {}..{}", p.rsi_len, bars.len(), live, bars[0].time, bars[bars.len() - 1].time);
    println!("rules: long close>SMA{} & RSI<{}; short close<SMA{} & RSI>{}; exit on SMA{} cross; fill next open\n",
        p.trend_len, p.buy_below, p.trend_len, p.sell_above, p.exit_len);

    let (acts, rsi, trend, exit) = simulate(&bars, &p);
    let next_open = |i: usize| bars.get(i + 1).map(|b| b.open);
    let ref_rt = round_trips(&acts, &next_open);

    if !ea_mode {
        summary("reference (Pine rules)", &ref_rt, pip);
        println!("\n  first 5 signals:");
        for (i, a) in acts.iter().take(5) { println!("    {}  {:<11} fill next open {:?}", bars[*i].time, a.name(), next_open(*i)); }
        return ExitCode::SUCCESS;
    }

    // ---- 1) indicator parity on every evaluated bar
    let (mut dr, mut dt, mut de, mut nchk) = (0.0_f64, 0.0_f64, 0.0_f64, 0usize);
    for (i, b) in bars.iter().enumerate() {
        if let (true, Some((r, t, e))) = (b.live, b.ea) {
            if rsi[i].is_nan() { continue; }
            dr = dr.max((r - rsi[i]).abs());
            dt = dt.max((t - trend[i]).abs());
            de = de.max((e - exit[i]).abs());
            nchk += 1;
        }
    }
    println!("1) INDICATORS on {nchk} evaluated bars — max |EA - reference|:");
    println!("   RSI {dr:.2e}   SMA{} {dt:.2e}   SMA{} {de:.2e}   {}", p.trend_len, p.exit_len,
        if dr < 1e-6 && dt < 1e-9 && de < 1e-9 { "MATCH" } else { "CHECK" });

    // ---- 2) trade-by-trade
    let tpath = arg(&argv, "--trades").unwrap_or_default();
    let ttext = match std::fs::read_to_string(&tpath) { Ok(t) => t, Err(e) => { eprintln!("{tpath}: {e}"); return ExitCode::FAILURE; } };
    let idx: BTreeMap<&str, usize> = bars.iter().enumerate().map(|(i, b)| (b.time.as_str(), i)).collect();
    let mut ea_acts = Vec::new(); // (signal idx or None, action, ref_open, fill)
    let mut ea_raw = Vec::new();
    for line in ttext.lines().skip(1) {
        let f: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if f.len() < 5 { continue; }
        let Some(a) = Act::parse(f[2]) else { continue };
        let (ro, fp) = (f[3].parse::<f64>().unwrap_or(f64::NAN), f[4].parse::<f64>().unwrap_or(f64::NAN));
        if !from.is_empty() && f[0] < from.as_str() { continue; }
        ea_raw.push((f[0].to_string(), a, ro, fp));
        if let Some(&i) = idx.get(f[0]) { ea_acts.push((i, a, ro, fp)); }
    }
    let refset: BTreeMap<(usize, &str), ()> = acts.iter().map(|(i, a)| ((*i, a.name()), ())).collect();
    let easet: BTreeMap<(usize, &str), (f64, f64)> = ea_acts.iter().map(|(i, a, r, f)| ((*i, a.name()), (*r, *f))).collect();
    let matched = refset.keys().filter(|k| easet.contains_key(*k)).count();
    let only_ref: Vec<_> = refset.keys().filter(|k| !easet.contains_key(*k)).collect();
    let only_ea: Vec<_> = easet.keys().filter(|k| !refset.contains_key(*k)).collect();
    let unplaced = ea_raw.len() - ea_acts.len();
    println!("\n2) TRADES — reference {} actions, EA {} actions, matched {} ({:.1}%)",
        acts.len(), ea_raw.len(), matched, 100.0 * matched as f64 / acts.len().max(1) as f64);
    println!("   only in reference: {}   only in EA: {}   EA rows on unknown bars: {unplaced}", only_ref.len(), only_ea.len());
    for (i, a) in only_ref.iter().take(10) { println!("     ref-only  {}  {a}", bars[*i].time); }
    for (i, a) in only_ea.iter().take(10) { println!("     EA-only   {}  {a}", bars[*i].time); }

    // ---- 3) prices: reference next-open vs EA's own open, and the cost of real fills
    let (mut dopen, mut nopen) = (0.0_f64, 0usize);
    let (mut buy_slip, mut nb, mut sell_slip, mut ns) = (0.0, 0usize, 0.0, 0usize);
    for ((i, name), (ro, fp)) in &easet {
        if let Some(o) = next_open(*i) { dopen = dopen.max((o - ro).abs()); nopen += 1; }
        if fp.is_finite() && ro.is_finite() {
            let s = fp - ro;
            if *name == "BUY" || *name == "CLOSE_SHORT" { buy_slip += s; nb += 1; } else { sell_slip += -s; ns += 1; }
        }
    }
    println!("\n3) PRICES — max |reference next open - EA bar open| over {nopen} fills: {dopen:.2e} {}",
        if dopen < 1e-9 { "MATCH" } else { "CHECK" });
    println!("   execution cost vs bar open: buys {:+.2} pips avg ({nb}), sells {:+.2} pips avg ({ns})  <- spread (+ price drift if execution is delayed), not a logic difference",
        if nb > 0 { buy_slip / nb as f64 / pip } else { 0.0 }, if ns > 0 { sell_slip / ns as f64 / pip } else { 0.0 });

    // ---- 4) performance: Pine-equivalent (open fills) vs what the EA actually got
    println!("\n4) PERFORMANCE (closed round trips):");
    summary("reference @ bar opens", &ref_rt, pip);
    let ea_pairs: Vec<(usize, Act)> = ea_acts.iter().map(|(i, a, _, _)| (*i, *a)).collect();
    let fillmap: BTreeMap<usize, f64> = ea_acts.iter().map(|(i, _, _, f)| (*i, *f)).collect();
    let ea_rt = round_trips(&ea_pairs, &|i| fillmap.get(&i).copied());
    summary("EA @ actual fills", &ea_rt, pip);

    let ok = matched == acts.len() && only_ea.is_empty() && dr < 1e-6 && dopen < 1e-9;
    println!("\nVERDICT: {}", if ok { "LOGIC PARITY — every signal and fill bar identical" } else { "MISMATCH — see lists above" });
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
