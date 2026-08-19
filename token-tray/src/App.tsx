import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type TokenTotals = {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  totalTokens: number;
};

type UsageSnapshot = {
  today: TokenTotals;
};

const emptyTotals: TokenTotals = {
  requests: 0,
  inputTokens: 0,
  outputTokens: 0,
  cacheReadTokens: 0,
  cacheCreationTokens: 0,
  totalTokens: 0,
};

function formatTokens(value: number) {
  return value.toLocaleString("en-US");
}

function App() {
  const [today, setToday] = useState<TokenTotals>(emptyTotals);
  const [displayedTokens, setDisplayedTokens] = useState(0);
  const [error, setError] = useState("");
  const displayedTokensRef = useRef(0);

  const refresh = useCallback(async () => {
    try {
      const snapshot = await invoke<UsageSnapshot>("get_usage_snapshot");
      setToday(snapshot.today);
      setError("");
    } catch (reason) {
      setError(String(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 30_000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    const startValue = displayedTokensRef.current;
    const targetValue = today.totalTokens;
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
  }, [today.totalTokens]);

  return (
    <main className="widget" title={error || "Today's token usage"}>
      <strong>{formatTokens(displayedTokens)}</strong>
    </main>
  );
}

export default App;
