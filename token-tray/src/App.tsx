import { useCallback, useEffect, useRef, useState, type MouseEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import "./App.css";

type TokenTotals = {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  totalTokens: number;
};

type DailyUsage = {
  date: string;
  totalTokens: number;
  requests: number;
};

type UsageSnapshot = {
  today: TokenTotals;
  month: TokenTotals;
  total: TokenTotals;
  lastSevenDays: TokenTotals;
  daily: DailyUsage[];
  updatedAt: string;
  source: string;
};

type BalanceSnapshot = {
  configured: boolean;
  name: string;
  remaining: number | null;
  unit: string;
  updatedAt: number | null;
  configPath: string;
  error: string | null;
};

type UsageUpdate = {
  snapshot: UsageSnapshot;
  lastSyncedAt: number | null;
  error: string | null;
};

type DetailsPanelProps = {
  snapshot: UsageSnapshot;
  balance: BalanceSnapshot;
  error: string;
  lastSyncedAt: number | null;
  onRefresh: () => Promise<void>;
  onRefreshBalance: () => Promise<void>;
  onOpenBalanceConfig: () => Promise<void>;
};

const currentAppWindow = getCurrentWindow();
const currentWindowLabel = currentAppWindow.label;

const emptyTotals: TokenTotals = {
  requests: 0,
  inputTokens: 0,
  outputTokens: 0,
  cacheReadTokens: 0,
  cacheCreationTokens: 0,
  totalTokens: 0,
};

const emptySnapshot: UsageSnapshot = {
  today: emptyTotals,
  month: emptyTotals,
  total: emptyTotals,
  lastSevenDays: emptyTotals,
  daily: [],
  updatedAt: "",
  source: "",
};

const emptyBalance: BalanceSnapshot = {
  configured: false,
  name: "自定义余额",
  remaining: null,
  unit: "",
  updatedAt: null,
  configPath: "",
  error: null,
};

function formatTokens(value: number) {
  return Math.max(0, value).toLocaleString("en-US");
}

function formatBalance(value: number) {
  return value.toLocaleString("en-US", {
    maximumFractionDigits: 6,
  });
}

function formatCacheHitRate(totals: TokenTotals) {
  const inputTokens = Math.max(0, totals.inputTokens);
  const cacheReadTokens = Math.max(0, totals.cacheReadTokens);
  const cacheCreationTokens = Math.max(0, totals.cacheCreationTokens);
  const cacheableInputTokens = inputTokens + cacheReadTokens + cacheCreationTokens;
  if (cacheableInputTokens === 0) return "—";

  const rate = (cacheReadTokens / cacheableInputTokens) * 100;
  return `${rate.toFixed(rate >= 10 ? 0 : 1)}%`;
}

function formatTokenApproximation(value: number) {
  const tokens = Math.max(0, value);
  const unit = tokens >= 100_000_000 ? 100_000_000 : tokens >= 10_000 ? 10_000 : 1;
  const unitLabel = unit === 100_000_000 ? "亿" : unit === 10_000 ? "万" : "";
  const amount = unit === 1 ? formatTokens(tokens) : (tokens / unit).toFixed(2).replace(/\.?0+$/, "");
  return `≈ ${amount}${unitLabel}`;
}

function formatSyncTime(timestamp: number | null) {
  if (!timestamp) return "等待首次同步";
  return new Date(timestamp).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function formatChartDate(value: string) {
  const parts = value.split("-");
  return parts.length >= 3 ? `${parts[1]}/${parts[2]}` : "--/--";
}

function formatChartScale(value: number) {
  if (value >= 100_000_000) return `${(value / 100_000_000).toFixed(value >= 1_000_000_000 ? 0 : 1).replace(/\.0$/, "")}亿`;
  if (value >= 10_000) return `${(value / 10_000).toFixed(value >= 100_000 ? 0 : 1).replace(/\.0$/, "")}万`;
  if (value >= 1_000) return `${Math.round(value / 1_000)}k`;
  return formatTokens(value);
}

function getChartAxisMax(value: number) {
  const target = Math.max(1, value);
  const magnitude = 10 ** Math.floor(Math.log10(target));
  const normalized = target / magnitude;
  const step = normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return step * magnitude;
}

function UsageTrendChart({ points: sourcePoints }: { points: DailyUsage[] }) {
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null);
  const points = sourcePoints.length > 0 ? sourcePoints.slice(-7) : Array.from({ length: 7 }, () => ({
    date: "",
    totalTokens: 0,
    requests: 0,
  }));
  const width = 340;
  const height = 174;
  const plotLeft = 38;
  const plotRight = 10;
  const plotTop = 12;
  const plotBottom = 34;
  const plotWidth = width - plotLeft - plotRight;
  const plotHeight = height - plotTop - plotBottom;
  const axisMax = getChartAxisMax(Math.max(...points.map((point) => point.totalTokens)));
  const coordinates = points.map((point, index) => ({
    ...point,
    x: plotLeft + (plotWidth * index) / Math.max(points.length - 1, 1),
    y: plotTop + plotHeight * (1 - point.totalTokens / axisMax),
  }));
  const linePoints = coordinates.map((point) => `${point.x},${point.y}`).join(" ");
  const firstPoint = coordinates[0];
  const lastPoint = coordinates[coordinates.length - 1];
  const areaPath = `M ${firstPoint.x} ${plotTop + plotHeight} L ${linePoints.replace(/ /g, " L ")} L ${lastPoint.x} ${plotTop + plotHeight} Z`;
  const hoveredPoint = hoveredIndex === null ? null : coordinates[hoveredIndex];
  const tooltipWidth = 148;
  const tooltipHeight = 55;
  const tooltipX = hoveredPoint ? Math.min(Math.max(hoveredPoint.x - tooltipWidth / 2, plotLeft), width - tooltipWidth - 4) : 0;
  const tooltipY = hoveredPoint ? Math.max(2, hoveredPoint.y - tooltipHeight - 10) : 0;
  const handleChartMouseMove = (event: MouseEvent<SVGSVGElement>) => {
    const chartRect = event.currentTarget.getBoundingClientRect();
    if (chartRect.width === 0) return;

    const chartX = ((event.clientX - chartRect.left) / chartRect.width) * width;
    const clampedX = Math.min(Math.max(chartX, plotLeft), width - plotRight);
    let nearestIndex = 0;
    let nearestDistance = Number.POSITIVE_INFINITY;
    coordinates.forEach((point, index) => {
      const distance = Math.abs(point.x - clampedX);
      if (distance < nearestDistance) {
        nearestIndex = index;
        nearestDistance = distance;
      }
    });
    setHoveredIndex(nearestIndex);
  };

  return (
    <div className="trend-card">
      <svg
        className="trend-chart"
        viewBox={`0 0 ${width} ${height}`}
        role="img"
        aria-label="近七天 token 使用趋势"
        onMouseMove={handleChartMouseMove}
        onMouseLeave={() => setHoveredIndex(null)}
      >
        <defs>
          <linearGradient id="trend-area-fill" x1="0" x2="0" y1="0" y2="1">
            <stop offset="0%" stopColor="#3e9fd1" stopOpacity=".3" />
            <stop offset="100%" stopColor="#3e9fd1" stopOpacity=".02" />
          </linearGradient>
        </defs>

        {[axisMax, axisMax / 2, 0].map((value) => {
          const y = plotTop + plotHeight * (1 - value / axisMax);
          return (
            <g key={value}>
              <line className="trend-gridline" x1={plotLeft} x2={width - plotRight} y1={y} y2={y} />
              <text className="trend-y-label" x={plotLeft - 7} y={y + 3} textAnchor="end">{formatChartScale(value)}</text>
            </g>
          );
        })}

        {hoveredPoint && <line className="trend-hover-line" x1={hoveredPoint.x} x2={hoveredPoint.x} y1={plotTop} y2={plotTop + plotHeight} />}
        <path className="trend-area" d={areaPath} />
        <polyline className="trend-line" points={linePoints} />

        {coordinates.map((point, index) => (
          <g key={`${point.date}-${index}`}>
            <circle
              className={hoveredIndex === index ? "trend-point is-active" : "trend-point"}
              cx={point.x}
              cy={point.y}
              r={hoveredIndex === index ? 4.5 : 3}
              tabIndex={0}
              onFocus={() => setHoveredIndex(index)}
              onBlur={() => setHoveredIndex(null)}
              aria-label={`${formatChartDate(point.date)} ${formatTokens(point.totalTokens)} tokens`}
            />
            <text className="trend-x-label" x={point.x} y={height - 9} textAnchor="middle">{formatChartDate(point.date)}</text>
          </g>
        ))}

        {hoveredPoint && (
          <g className="trend-tooltip" pointerEvents="none">
            <rect x={tooltipX} y={tooltipY} width={tooltipWidth} height={tooltipHeight} rx="9" />
            <text className="trend-tooltip-date" x={tooltipX + 10} y={tooltipY + 16}>{formatChartDate(hoveredPoint.date)}</text>
            <text className="trend-tooltip-value" x={tooltipX + 10} y={tooltipY + 32}>{formatTokens(hoveredPoint.totalTokens)} tokens</text>
            <text className="trend-tooltip-approximation" x={tooltipX + 10} y={tooltipY + 47}>{formatTokenApproximation(hoveredPoint.totalTokens)} · {hoveredPoint.requests} 次</text>
          </g>
        )}
      </svg>
    </div>
  );
}

function DetailsPanel({
  snapshot,
  balance,
  error,
  lastSyncedAt,
  onRefresh,
  onRefreshBalance,
  onOpenBalanceConfig,
}: DetailsPanelProps) {
  const { today } = snapshot;
  const overview = [
    { label: "近 7 天", value: snapshot.lastSevenDays.totalTokens },
    { label: "本月", value: snapshot.month.totalTokens },
    { label: "累计", value: snapshot.total.totalTokens },
  ];

  return (
    <main className="details-page">
      <header className="details-header">
        <div>
          <span className="details-kicker">Token 统计</span>
          <h1>今日用量</h1>
        </div>
        <button className="close-button" type="button" onClick={() => void invoke("hide_details_window")} aria-label="关闭详情面板">
          ×
        </button>
      </header>

      <section className="details-hero">
        <div className="details-total">
          <strong>{formatTokens(today.totalTokens)}</strong>
          <span className="token-approximation">{formatTokenApproximation(today.totalTokens)}</span>
        </div>
        <span>tokens</span>
        <p>{today.requests.toLocaleString("en-US")} 次请求</p>
      </section>

      <section className="balance-card" aria-label="余额" aria-live="polite">
        <div className="balance-card-heading">
          <div>
            <span className="balance-kicker">余额</span>
            <h2>{balance.name}</h2>
          </div>
          <div className="balance-actions">
            <button className="balance-action" type="button" onClick={() => void onRefreshBalance()}>
              刷新
            </button>
            <button
              className="balance-action is-muted"
              type="button"
              onClick={() => void onOpenBalanceConfig()}
              title={balance.configPath || undefined}
            >
              配置
            </button>
          </div>
        </div>
        {balance.remaining !== null ? (
          <div className="balance-value">
            <strong>{formatBalance(balance.remaining)}</strong>
            <span>{balance.unit || "USD"}</span>
          </div>
        ) : (
          <p className="balance-empty">
            {balance.configured ? "暂时无法读取余额" : "尚未配置余额接口"}
          </p>
        )}
        <p className={balance.error ? "balance-meta is-error" : "balance-meta"} title={balance.configPath || undefined}>
          {balance.error
            ? balance.error
            : balance.updatedAt
              ? `最近更新 ${formatSyncTime(balance.updatedAt)}`
              : "点击“配置”创建 balance.json"}
        </p>
      </section>

      <section className="overview-grid" aria-label="用量概览">
        {overview.map((item) => (
          <div className="overview-card" key={item.label}>
            <span>{item.label}</span>
            <strong>{formatTokens(item.value)}</strong>
          </div>
        ))}
      </section>

      <section className="details-section trend-section">
        <div className="section-heading">
          <h2>近七天趋势</h2>
          <span>{formatTokens(snapshot.lastSevenDays.totalTokens)} tokens · {formatTokenApproximation(snapshot.lastSevenDays.totalTokens)}</span>
        </div>
        <UsageTrendChart points={snapshot.daily} />
      </section>

      <section className="details-section">
        <div className="section-heading">
          <h2>今日明细</h2>
          <span>{today.requests.toLocaleString("en-US")} 次请求</span>
        </div>
        <div className="metric-list">
          <div className="metric-row"><span>输入 token</span><strong>{formatTokens(today.inputTokens)}</strong></div>
          <div className="metric-row"><span>输出 token</span><strong>{formatTokens(today.outputTokens)}</strong></div>
          <div className="metric-row"><span>读取缓存</span><strong>{formatTokens(today.cacheReadTokens)}</strong></div>
          <div className="metric-row"><span>缓存命中率</span><strong>{formatCacheHitRate(today)}</strong></div>
        </div>
      </section>

      <footer className="details-footer">
        <span className={error ? "sync-state is-error" : "sync-state"} title={error || undefined}>
          {error ? "同步失败，保留上次数据" : `最近同步 ${formatSyncTime(lastSyncedAt)}`}
        </span>
        <button className="refresh-button" type="button" onClick={() => void onRefresh()}>
          刷新
        </button>
      </footer>
    </main>
  );
}

function App() {
  const [snapshot, setSnapshot] = useState<UsageSnapshot>(emptySnapshot);
  const [balance, setBalance] = useState<BalanceSnapshot>(emptyBalance);
  const [displayedTokens, setDisplayedTokens] = useState(0);
  const [error, setError] = useState("");
  const [lastSyncedAt, setLastSyncedAt] = useState<number | null>(null);
  const displayedTokensRef = useRef(0);

  const applyUpdate = useCallback((update: UsageUpdate) => {
    setSnapshot(update.snapshot);
    setLastSyncedAt(update.lastSyncedAt);
    setError(update.error ?? "");
  }, []);

  const readCached = useCallback(async () => {
    try {
      const update = await invoke<UsageUpdate>("get_usage_snapshot");
      applyUpdate(update);
    } catch (reason) {
      setError(String(reason));
    }
  }, [applyUpdate]);

  const readBalance = useCallback(async () => {
    try {
      const result = await invoke<BalanceSnapshot>("get_balance");
      setBalance(result);
    } catch (_reason) {
      setBalance((current) => ({ ...current, error: "读取余额失败" }));
    }
  }, []);

  const refresh = useCallback(async () => {
    try {
      const update = await invoke<UsageUpdate>("sync_usage_now");
      applyUpdate(update);
    } catch (reason) {
      setError(String(reason));
    }
    await readBalance();
  }, [applyUpdate, readBalance]);

  const openBalanceConfig = useCallback(async () => {
    try {
      const configPath = await invoke<string>("open_balance_config");
      setBalance((current) => ({ ...current, configPath, error: null }));
    } catch (_reason) {
      setBalance((current) => ({ ...current, error: "无法打开余额配置文件" }));
    }
  }, []);

  useEffect(() => {
    document.body.dataset.window = currentWindowLabel;
    return () => {
      delete document.body.dataset.window;
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    const setup = async () => {
      try {
        const dispose = await listen<UsageUpdate>("usage-updated", (event) => {
          applyUpdate(event.payload);
        });
        if (disposed) {
          dispose();
          return;
        }
        unlisten = dispose;
        await readCached();
      } catch (reason) {
        setError(String(reason));
      }
    };
    void setup();

    const refreshWhenActive = () => {
      if (document.visibilityState === "visible") void readCached();
    };
    window.addEventListener("focus", refreshWhenActive);
    document.addEventListener("visibilitychange", refreshWhenActive);

    return () => {
      disposed = true;
      unlisten?.();
      window.removeEventListener("focus", refreshWhenActive);
      document.removeEventListener("visibilitychange", refreshWhenActive);
    };
  }, [applyUpdate, readCached]);

  useEffect(() => {
    if (currentWindowLabel !== "details") return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        void invoke("hide_details_window");
      }
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, []);

  useEffect(() => {
    if (currentWindowLabel !== "details") return;

    void readBalance();
    const interval = window.setInterval(() => {
      void readBalance();
    }, 30_000);
    return () => window.clearInterval(interval);
  }, [readBalance]);

  useEffect(() => {
    if (currentWindowLabel !== "main" || import.meta.env.DEV) return;
    let disposed = false;
    const checkForUpdate = async () => {
      try {
        const update = await check();
        if (!update || disposed) return;
        await update.downloadAndInstall();
        if (!disposed) await relaunch();
      } catch (_reason) {
        console.warn("版本检查失败");
      }
    };
    void checkForUpdate();
    return () => {
      disposed = true;
    };
  }, []);

  useEffect(() => {
    const startValue = displayedTokensRef.current;
    const targetValue = snapshot.today.totalTokens;
    if (startValue === targetValue) return;

    const difference = Math.abs(targetValue - startValue);
    const duration = Math.min(1800, Math.max(450, Math.log10(difference + 1) * 220));
    let startedAt = 0;
    let frame = 0;

    const animate = (timestamp: number) => {
      if (startedAt === 0) startedAt = timestamp;
      const progress = Math.min((timestamp - startedAt) / duration, 1);
      const easedProgress = 1 - Math.pow(1 - progress, 3);
      const nextValue = Math.round(startValue + (targetValue - startValue) * easedProgress);
      displayedTokensRef.current = nextValue;
      setDisplayedTokens(nextValue);
      if (progress < 1) frame = requestAnimationFrame(animate);
    };

    frame = requestAnimationFrame(animate);
    return () => cancelAnimationFrame(frame);
  }, [snapshot.today.totalTokens]);

  const toggleDetails = useCallback(async () => {
    try {
      await invoke("toggle_details_window");
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  if (currentWindowLabel === "details") {
    return (
      <DetailsPanel
        snapshot={snapshot}
        balance={balance}
        error={error}
        lastSyncedAt={lastSyncedAt}
        onRefresh={refresh}
        onRefreshBalance={readBalance}
        onOpenBalanceConfig={openBalanceConfig}
      />
    );
  }

  const title = error
    ? `同步失败，保留上次成功数据：${error}`
    : `今日 token：${formatTokens(snapshot.today.totalTokens)} · 最近同步 ${formatSyncTime(lastSyncedAt)}`;

  return (
    <main
      className="widget"
      title={title}
      aria-label="打开或关闭今日 token 详情"
      onClick={() => void toggleDetails()}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          void toggleDetails();
        }
      }}
      role="button"
      tabIndex={0}
    >
      <strong>{formatTokens(displayedTokens)}</strong>
      <span className="widget-approximation">{formatTokenApproximation(displayedTokens)}</span>
    </main>
  );
}

export default App;
