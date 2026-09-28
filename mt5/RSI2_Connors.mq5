//+------------------------------------------------------------------+
//| RSI(2) mean reversion (Connors) — MT5 port of RSI2_Connors.pine   |
//|                                                                  |
//| Parity rules with the TradingView original:                      |
//|  * signals use CLOSED bars only; orders go out at the next bar's |
//|    open (Pine default, process_orders_on_close = false)          |
//|  * RSI is Pine's ta.rsi: Wilder/RMA smoothing, seeded with the   |
//|    SMA of the first `len` changes, down==0 -> 100, up==0 -> 0.   |
//|    Computed here from closes, not iRSI, so the maths is identical|
//|  * one decision per bar in the same order as the Pine `if` chain |
//|  * the decision is made at the bar open but EXECUTED at the      |
//|    first tick inside a trade session: many brokers are closed at |
//|    00:00 (daily rollover) and reject orders sent at the bar open |
//|  * optional CSV log (bars + trades) in Common\Files for the      |
//|    trade-by-trade parity check                                   |
//+------------------------------------------------------------------+
#property copyright "RSI2 conversion portfolio piece"
#property version   "1.00"

#include <Trade\Trade.mqh>

input int    InpRsiLen    = 2;      // RSI length
input double InpBuyBelow  = 5.0;    // Buy when RSI below
input double InpSellAbove = 95.0;   // Short when RSI above
input int    InpTrendLen  = 200;    // Trend SMA length
input int    InpExitLen   = 5;      // Exit SMA length
input bool   InpAllowShort = true;  // Allow shorts
input double InpLots      = 1.0;    // Lots (1.0 = 100,000 units, matches the Pine qty)
input ulong  InpMagic     = 20260928;
input int    InpDelayMinutes = 0;   // Wait this long after the bar open (avoids wide rollover spreads)
input bool   InpLogCsv    = true;   // Write parity log to Common\Files

enum Pending { PEND_NONE, PEND_BUY, PEND_SELL, PEND_CLOSE_LONG, PEND_CLOSE_SHORT };

CTrade   trade;
datetime g_lastBar = 0;
bool     g_dumped = false;
int      g_barsFile = INVALID_HANDLE;
int      g_tradesFile = INVALID_HANDLE;
Pending  g_pending = PEND_NONE;     // decision waiting for a tradeable tick
datetime g_pendingSignal = 0;       // the closed bar that produced it
double   g_pendingRefOpen = 0.0;    // open of the bar it belongs to (Pine's fill price)

string PendingName(const Pending p)
{
   switch(p)
   {
      case PEND_BUY:         return "BUY";
      case PEND_SELL:        return "SELL";
      case PEND_CLOSE_LONG:  return "CLOSE_LONG";
      case PEND_CLOSE_SHORT: return "CLOSE_SHORT";
      default:               return "NONE";
   }
}

//--- true when the broker's trade session for this symbol is open now
bool InTradeSession()
{
   MqlDateTime now;
   TimeToStruct(TimeCurrent(), now);
   int sec = now.hour * 3600 + now.min * 60 + now.sec;
   datetime from, to;
   for(uint i = 0; SymbolInfoSessionTrade(_Symbol, (ENUM_DAY_OF_WEEK)now.day_of_week, i, from, to); i++)
   {
      int f = (int)(from % 86400), t = (int)(to % 86400);
      if(t == 0) t = 86400; // a session ending at 24:00
      if(sec >= f && sec < t) return true;
   }
   return false;
}

//--- Pine ta.rsi over closes[0..n-1] (oldest first); returns the value at the last element
double PineRsi(const double &c[], const int n, const int len)
{
   if(n < len + 1) return EMPTY_VALUE;
   double up = 0.0, dn = 0.0;
   // seed: SMA of the first `len` changes (Pine ta.rma seeds with ta.sma)
   for(int i = 1; i <= len; i++)
   {
      double ch = c[i] - c[i - 1];
      up += MathMax(ch, 0.0);
      dn += MathMax(-ch, 0.0);
   }
   up /= len;
   dn /= len;
   double a = 1.0 / len;
   for(int i = len + 1; i < n; i++)
   {
      double ch = c[i] - c[i - 1];
      up = a * MathMax(ch, 0.0) + (1.0 - a) * up;
      dn = a * MathMax(-ch, 0.0) + (1.0 - a) * dn;
   }
   if(dn == 0.0) return 100.0;
   if(up == 0.0) return 0.0;
   return 100.0 - 100.0 / (1.0 + up / dn);
}

double Sma(const double &c[], const int n, const int len)
{
   if(n < len) return EMPTY_VALUE;
   double s = 0.0;
   for(int i = n - len; i < n; i++) s += c[i];
   return s / len;
}

//--- +1 long, -1 short, 0 flat (this EA's positions on this symbol only)
int PositionSide()
{
   for(int i = PositionsTotal() - 1; i >= 0; i--)
   {
      ulong ticket = PositionGetTicket(i);
      if(ticket == 0) continue;
      if(PositionGetString(POSITION_SYMBOL) != _Symbol) continue;
      if((ulong)PositionGetInteger(POSITION_MAGIC) != InpMagic) continue;
      return PositionGetInteger(POSITION_TYPE) == POSITION_TYPE_BUY ? 1 : -1;
   }
   return 0;
}

bool ClosePositions()
{
   bool ok = true;
   for(int i = PositionsTotal() - 1; i >= 0; i--)
   {
      ulong ticket = PositionGetTicket(i);
      if(ticket == 0) continue;
      if(PositionGetString(POSITION_SYMBOL) != _Symbol) continue;
      if((ulong)PositionGetInteger(POSITION_MAGIC) != InpMagic) continue;
      ok = trade.PositionClose(ticket) && ok;
   }
   return ok;
}

void LogBar(const MqlRates &r, const string phase, const double rsi, const double trend, const double exitMa)
{
   if(g_barsFile == INVALID_HANDLE) return;
   FileWrite(g_barsFile, TimeToString(r.time, TIME_DATE | TIME_MINUTES), phase,
             DoubleToString(r.open, 8), DoubleToString(r.high, 8), DoubleToString(r.low, 8), DoubleToString(r.close, 8),
             rsi == EMPTY_VALUE ? "" : DoubleToString(rsi, 10),
             trend == EMPTY_VALUE ? "" : DoubleToString(trend, 10),
             exitMa == EMPTY_VALUE ? "" : DoubleToString(exitMa, 10));
}

void LogTrade(const datetime signalBar, const string action, const double refPrice, const double fillPrice)
{
   if(g_tradesFile == INVALID_HANDLE) return;
   FileWrite(g_tradesFile, TimeToString(signalBar, TIME_DATE | TIME_MINUTES),
             TimeToString(iTime(_Symbol, _Period, 0), TIME_DATE | TIME_MINUTES), action,
             DoubleToString(refPrice, 8), DoubleToString(fillPrice, 8),
             TimeToString(TimeCurrent(), TIME_DATE | TIME_SECONDS));
}

//--- try to execute the pending decision; keeps it pending while the market is closed
void TryExecute()
{
   if(g_pending == PEND_NONE) return;
   if(TimeCurrent() < iTime(_Symbol, _Period, 0) + InpDelayMinutes * 60) return;
   if(!InTradeSession()) return;
   bool ok = false;
   switch(g_pending)
   {
      case PEND_BUY:         ok = trade.Buy(InpLots, _Symbol); break;
      case PEND_SELL:        ok = trade.Sell(InpLots, _Symbol); break;
      case PEND_CLOSE_LONG:
      case PEND_CLOSE_SHORT: ok = ClosePositions(); break;
      default: break;
   }
   if(ok)
   {
      LogTrade(g_pendingSignal, PendingName(g_pending), g_pendingRefOpen, trade.ResultPrice());
      g_pending = PEND_NONE;
   }
   else if(trade.ResultRetcode() != TRADE_RETCODE_MARKET_CLOSED)
      PrintFormat("%s failed (retcode %u) — will retry next tick", PendingName(g_pending), trade.ResultRetcode());
}

int OnInit()
{
   if(InpRsiLen < 1 || InpTrendLen < 1 || InpExitLen < 1) return INIT_PARAMETERS_INCORRECT;
   trade.SetExpertMagicNumber(InpMagic);
   trade.SetTypeFillingBySymbol(_Symbol);
   if(InpLogCsv)
   {
      string tag = _Symbol + "_" + EnumToString(_Period);
      g_barsFile = FileOpen("rsi2_bars_" + tag + ".csv", FILE_WRITE | FILE_CSV | FILE_ANSI | FILE_COMMON, ',');
      g_tradesFile = FileOpen("rsi2_trades_" + tag + ".csv", FILE_WRITE | FILE_CSV | FILE_ANSI | FILE_COMMON, ',');
      if(g_barsFile == INVALID_HANDLE || g_tradesFile == INVALID_HANDLE)
         Print("parity log disabled: cannot open files in Common\\Files, error ", GetLastError());
      else
      {
         FileWrite(g_barsFile, "time", "phase", "open", "high", "low", "close", "ea_rsi", "ea_trend", "ea_exit");
         FileWrite(g_tradesFile, "signal_bar", "fill_bar", "action", "ref_open", "fill_price", "fill_time");
      }
   }
   return INIT_SUCCEEDED;
}

void OnDeinit(const int reason)
{
   if(g_barsFile != INVALID_HANDLE) FileClose(g_barsFile);
   if(g_tradesFile != INVALID_HANDLE) FileClose(g_tradesFile);
}

void OnTick()
{
   datetime barTime = iTime(_Symbol, _Period, 0);
   if(barTime == 0) return;
   if(barTime == g_lastBar) { TryExecute(); return; }
   g_lastBar = barTime;
   if(g_pending != PEND_NONE)
   {
      // a whole bar passed without a tradeable tick (e.g. a holiday): Pine would have filled, we could not
      PrintFormat("%s from %s expired unfilled — market never opened during that bar",
                  PendingName(g_pending), TimeToString(g_pendingSignal, TIME_DATE));
      g_pending = PEND_NONE;
   }

   // closed bars, oldest first; the RSI seed washes out long before `extra` bars
   int extra = 100;
   int need = MathMax(InpTrendLen, InpExitLen) + InpRsiLen + extra;
   MqlRates rates[];
   ArraySetAsSeries(rates, false);
   int got = CopyRates(_Symbol, _Period, 1, need, rates);
   if(got < need) return; // not enough history yet — Pine would also still be warming up the 200 SMA

   // first evaluated bar: dump the history the indicators are built on, so the parity tool can rebuild them
   if(!g_dumped && g_barsFile != INVALID_HANDLE)
   {
      g_dumped = true;
      MqlRates hist[];
      ArraySetAsSeries(hist, false);
      int all = Bars(_Symbol, _Period) - 1;
      int h = CopyRates(_Symbol, _Period, 1, all, hist);
      for(int i = 0; i < h - 1; i++) LogBar(hist[i], "H", EMPTY_VALUE, EMPTY_VALUE, EMPTY_VALUE);
   }

   double c[];
   ArrayResize(c, got);
   for(int i = 0; i < got; i++) c[i] = rates[i].close;
   double rsi    = PineRsi(c, got, InpRsiLen);
   double trend  = Sma(c, got, InpTrendLen);
   double exitMa = Sma(c, got, InpExitLen);
   MqlRates last = rates[got - 1];
   LogBar(last, "L", rsi, trend, exitMa);

   double close = last.close;
   int side = PositionSide();

   // same order as the Pine if/else-if chain
   Pending act = PEND_NONE;
   if(side == 0 && close > trend && rsi < InpBuyBelow)                          act = PEND_BUY;
   else if(side == 0 && InpAllowShort && close < trend && rsi > InpSellAbove)   act = PEND_SELL;
   else if(side > 0 && close > exitMa)                                          act = PEND_CLOSE_LONG;
   else if(side < 0 && close < exitMa)                                          act = PEND_CLOSE_SHORT;

   if(act != PEND_NONE)
   {
      g_pending = act;
      g_pendingSignal = last.time;
      g_pendingRefOpen = iOpen(_Symbol, _Period, 0);
      TryExecute();
   }
}
