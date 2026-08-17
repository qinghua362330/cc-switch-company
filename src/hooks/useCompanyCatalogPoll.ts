import { useEffect, useRef } from "react";

const CATALOG_REFRESH_HOUR = 10;
const CATALOG_REFRESH_SEED_DELAY_MS = 3 * 1000;
const CATALOG_REFRESH_LAST_RUN_KEY =
  "cc-switch-company-catalog-last-refresh-date-v1";

export function companyCatalogRefreshDateKey(date: Date): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

export function shouldRunCompanyCatalogRefresh(
  now: Date,
  lastRunDate: string | null,
): boolean {
  const scheduledAt = new Date(now);
  scheduledAt.setHours(CATALOG_REFRESH_HOUR, 0, 0, 0);
  return (
    now.getTime() >= scheduledAt.getTime() &&
    lastRunDate !== companyCatalogRefreshDateKey(now)
  );
}

export function nextCompanyCatalogRefreshDelayMs(
  now: Date,
  lastRunDate: string | null,
): number {
  if (shouldRunCompanyCatalogRefresh(now, lastRunDate)) {
    return CATALOG_REFRESH_SEED_DELAY_MS;
  }

  return nextScheduledCompanyCatalogRefreshDelayMs(now);
}

function nextScheduledCompanyCatalogRefreshDelayMs(now: Date): number {
  const nextRun = new Date(now);
  nextRun.setHours(CATALOG_REFRESH_HOUR, 0, 0, 0);
  if (nextRun.getTime() <= now.getTime()) {
    nextRun.setDate(nextRun.getDate() + 1);
  }
  return Math.max(0, nextRun.getTime() - now.getTime());
}

function readLastRefreshDate(): string | null {
  try {
    return window.localStorage.getItem(CATALOG_REFRESH_LAST_RUN_KEY);
  } catch {
    return null;
  }
}

function writeLastRefreshDate(date: Date): void {
  try {
    window.localStorage.setItem(
      CATALOG_REFRESH_LAST_RUN_KEY,
      companyCatalogRefreshDateKey(date),
    );
  } catch {
    // Storage failure only means the refresh may be retried after app restart.
  }
}

interface UseCompanyCatalogPollOptions {
  /** 仅在公司会话已登录时运行每日刷新。 */
  readonly enabled: boolean;
  /** 拉取线上目录、比较并按需重建 providers。 */
  readonly onDailyRefresh: () => void | Promise<void>;
}

/**
 * 每天本地时间 10:00 刷新公司模型目录。
 *
 * - 应用在 10:00 运行时准点执行。
 * - 当天 10:00 后首次启动或重新激活应用时补跑一次。
 * - 只有刷新成功才记录当天已执行；失败会在下次激活时重试。
 */
export function useCompanyCatalogPoll({
  enabled,
  onDailyRefresh,
}: UseCompanyCatalogPollOptions): void {
  const onDailyRefreshRef = useRef(onDailyRefresh);

  useEffect(() => {
    onDailyRefreshRef.current = onDailyRefresh;
  }, [onDailyRefresh]);

  useEffect(() => {
    if (!enabled) return;

    let active = true;
    let inFlight = false;
    let timerId: number | null = null;

    const clearTimer = () => {
      if (timerId === null) return;
      window.clearTimeout(timerId);
      timerId = null;
    };

    const schedule = (delayMs: number) => {
      if (!active) return;
      clearTimer();
      timerId = window.setTimeout(() => {
        timerId = null;
        void refresh();
      }, delayMs);
    };

    const scheduleNextDueRefresh = () => {
      const now = new Date();
      schedule(nextCompanyCatalogRefreshDelayMs(now, readLastRefreshDate()));
    };

    const scheduleNextDay = () => {
      const now = new Date();
      schedule(nextScheduledCompanyCatalogRefreshDelayMs(now));
    };

    const refresh = async () => {
      if (!active || inFlight) return;
      clearTimer();
      inFlight = true;
      try {
        await onDailyRefreshRef.current();
        if (!active) return;
        writeLastRefreshDate(new Date());
      } catch (error) {
        console.warn("[CompanyAuth] Daily catalog refresh failed", error);
      } finally {
        inFlight = false;
        scheduleNextDay();
      }
    };

    const handleAppActivated = () => {
      if (document.visibilityState !== "visible") return;

      const now = new Date();
      if (shouldRunCompanyCatalogRefresh(now, readLastRefreshDate())) {
        void refresh();
      }
    };

    window.addEventListener("focus", handleAppActivated);
    document.addEventListener("visibilitychange", handleAppActivated);
    scheduleNextDueRefresh();

    return () => {
      active = false;
      clearTimer();
      window.removeEventListener("focus", handleAppActivated);
      document.removeEventListener("visibilitychange", handleAppActivated);
    };
  }, [enabled]);
}
