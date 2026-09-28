# TradingView → MT5 conversion, with proof it matches

A TradingView Pine Script strategy converted to a MetaTrader 5 Expert Advisor,
plus an independent checker that proves the two make **the same trades on the
same bars** — and a report of what the conversion uncovered about the broker.

**Result: 46 of 46 trade actions identical, none missing, none extra.**
Indicators agree to 11 decimal places. → [Full parity report](REPORT.md)

## Why this matters

Most conversions are "close enough": the MT5 version *looks* like the TradingView
one until you compare trades. The usual causes are small and invisible:

- **Order timing.** TradingView acts on the bar close and fills at the next bar's
  open. Ports that trade mid-bar or on the close drift away immediately.
- **Indicator start-up.** MT5's built-in RSI starts its smoothing differently from
  TradingView's `ta.rsi`, which flips trades sitting right on a threshold.
- **The broker itself.** Session times, rollover spreads, and gaps in price history
  change what actually happens live.

This repo handles all three and **measures** the result instead of eyeballing it.

## What the backtest uncovered

| finding | impact | fix |
|---|---|---|
| Broker is closed for the first 3 minutes of each day | A direct port made **zero trades** — every order at 00:00 was rejected | Decide at the bar open, execute on the first tick inside the trade session |
| Broker's minute history starts in Oct 2022 | Earlier signals cannot be simulated faithfully | Comparison limited to the valid window, and documented |
| Rollover spreads | ≈5.8 pips per round trip when filling right after the open | Optional `InpDelayMinutes` input; 60 min roughly halved the cost (small sample, confirm live) |

## Contents

| path | what |
|---|---|
| [`pine/RSI2_Connors.pine`](pine/RSI2_Connors.pine) | the strategy (Pine v6): Connors RSI(2), 200-SMA trend filter, 5-SMA exit, long and short |
| [`mt5/RSI2_Connors.mq5`](mt5/RSI2_Connors.mq5) | the MT5 Expert Advisor — compiles with 0 errors, 0 warnings |
| [`mt5/tester.ini`](mt5/tester.ini) | Strategy Tester config (EURUSD.PRO, D1, 1-minute OHLC); `tester_delay60.ini` adds the 60-minute delay |
| [`parity/`](parity/src/main.rs) | the checker: re-implements the Pine rules from scratch (Rust, no dependencies) and matches every EA trade against them |
| [`results/`](results/) | the EA's bar and trade logs from both runs, and the checker's output |
| [`REPORT.md`](REPORT.md) | the client-facing parity report |

## How the check works

The EA writes every bar it evaluated and every trade it made to CSV. The checker
reads those exact bars, recomputes RSI and both moving averages the way TradingView
does, replays the strategy, and compares:

1. **Indicators** on every evaluated bar.
2. **Trade actions** — buy, sell, close — matched by signal bar.
3. **Fill prices** against the next bar's open (TradingView's rule), with the
   broker's real fill cost reported separately.

## Reproduce

1. Copy `mt5/RSI2_Connors.mq5` into `MQL5/Experts/Freelance/` and compile it in MetaEditor.
2. Close MetaTrader and run the tester headless:
   `terminal64.exe /config:"<full path>\mt5\tester.ini"`
   (edit `Symbol=` to your broker's name for EURUSD).
3. Run the checker on the logs the EA wrote to `MetaQuotes/Terminal/Common/Files`:

```bash
cargo run --release --manifest-path parity/Cargo.toml -- --bars results/open_asap/rsi2_bars_EURUSD.PRO_PERIOD_D1.csv --trades results/open_asap/rsi2_trades_EURUSD.PRO_PERIOD_D1.csv --from 2022.10.01
```

## Disclaimer

This demonstrates a correct conversion. It is not trading advice and makes no
claim that the strategy is profitable — 23 round trips is far too few to judge that.

License: [MIT](LICENSE)
