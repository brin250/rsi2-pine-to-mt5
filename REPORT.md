# Parity report — RSI(2) mean reversion, TradingView → MT5

**Verdict: logic parity confirmed.** On every bar where the broker's data allows a
faithful test, the MT5 Expert Advisor makes exactly the same decisions as the
TradingView strategy: 46 of 46 trade actions identical, none missing, none extra.

| | |
|---|---|
| Strategy | [`pine/RSI2_Connors.pine`](pine/RSI2_Connors.pine) (Pine v6) — Connors RSI(2), 200-SMA filter, 5-SMA exit, long + short |
| Port | [`mt5/RSI2_Connors.mq5`](mt5/RSI2_Connors.mq5) — compiles with 0 errors, 0 warnings |
| Broker / symbol | OxSecurities demo, `EURUSD.PRO`, daily bars |
| Tester | MT5 Strategy Tester, 1-minute OHLC model, 1.0 lot fixed, US$100,000 deposit (≈1.1× notional leverage) |
| Compared window | 2022-10-01 → 2026-09-24 (where the broker has minute data — see finding 2) |
| Checker | independent re-implementation of the Pine rules ([`parity/`](parity/src/main.rs)) run on the exact bars the EA saw |

## Results

| check | result |
|---|---|
| 1. Indicators on 1,035 bars | RSI max difference 5×10⁻¹¹, SMAs ≤ 2×10⁻¹⁵ — **match** |
| 2. Trade actions | reference 46, EA 46, **matched 46 (100%)**, 0 missing, 0 extra |
| 3. Reference fill price (next bar open) | identical on all 46 fills — **match** |

Raw output: [`results/open_asap/parity_output.txt`](results/open_asap/parity_output.txt),
[`results/delay_60min/parity_output.txt`](results/delay_60min/parity_output.txt).

Pine's order timing — signal on the bar close, fill at the next bar's open — is
reproduced exactly. RSI is computed the way TradingView computes it (`ta.rsi`,
Wilder smoothing with its specific start-up), not with MT5's built-in `iRSI`,
whose start-up differs and can flip trades that sit right on the 5 / 95 thresholds.

## Findings a straight conversion would have missed

**1. The broker rejects orders at the daily open.** This broker's EURUSD session
opens at 00:03 server time, so orders sent at 00:00 — the moment a new daily
bar starts — fail with *Market closed*. A direct port trades **zero times** in the
backtest and would fail the same way live. The EA now makes its decision at the
bar open and executes on the first tick inside the broker's trade session.

**2. The broker's minute data starts in October 2022.** Before that the tester has
only daily data and cannot simulate the 00:03 open, so earlier signals expire
unfilled (56 cases, 2016–2022). That is a data limitation of this demo server, not
an EA defect; the comparison window starts where the data is complete. A broker
with longer minute history would extend it.

**3. Filling right after rollover is expensive.** Spreads are widest just after the
daily rollover. Measured against the bar open:

| execution time | buys | sells | round trip | EA avg per trade | WR | trades |
|---|---|---|---|---|---|---|
| Pine reference (bar open, no costs) | — | — | — | +3.38 pips | 65.2% | 23 |
| 00:03 (as soon as the market opens) | +4.59 pips | +1.23 pips | ≈5.8 pips | −2.44 pips | 56.5% | 23 |
| 01:00 (`InpDelayMinutes = 60`) | +3.16 pips | −0.52 pips | ≈2.6 pips | +0.75 pips | 65.2% | 23 |

With a 60-minute delay the cost roughly halves. Caveat: with 23 trades, part of that
difference is ordinary price movement during the hour, so treat the delay as a
setting to confirm on a live demo, not a proven improvement. The delay is an input,
default 0, so behaviour stays identical to TradingView unless you choose otherwise.

## TradingView vs broker prices (separate from logic)

TradingView and a broker use different price feeds. Their daily candles can
differ slightly, which occasionally moves a signal by a bar. That's a data
difference, not a conversion error. A TradingView *List of Trades* export for the
same period lets each such trade be listed with its cause.

*This report checks that the conversion is correct. It is not trading advice and
makes no claim that the strategy is profitable; 23 round trips is far too few to
judge that.*
