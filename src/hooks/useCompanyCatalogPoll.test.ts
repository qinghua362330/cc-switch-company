import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  companyCatalogRefreshDateKey,
  nextCompanyCatalogRefreshDelayMs,
  shouldRunCompanyCatalogRefresh,
  useCompanyCatalogPoll,
} from "./useCompanyCatalogPoll";

const LAST_REFRESH_KEY = "cc-switch-company-catalog-last-refresh-date-v1";

function deferredPromise(): {
  promise: Promise<void>;
  resolve: () => void;
  reject: (error: unknown) => void;
} {
  let resolve!: () => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<void>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("useCompanyCatalogPoll", () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(2026, 7, 17, 10, 30, 0));
    window.localStorage.clear();
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "visible",
    });
    warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
    vi.useRealTimers();
  });

  it("waits until the next scheduled day after a failed refresh", async () => {
    const onDailyRefresh = vi.fn().mockRejectedValue(new Error("offline"));
    renderHook(() => useCompanyCatalogPoll({ enabled: true, onDailyRefresh }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();

    const nextScheduledRun = new Date(2026, 7, 18, 10, 0, 0).getTime();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(nextScheduledRun - Date.now() - 1);
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(onDailyRefresh).toHaveBeenCalledTimes(2);
  });

  it("retries on a later activation and suppresses repeated in-flight activations", async () => {
    const firstAttempt = deferredPromise();
    const onDailyRefresh = vi
      .fn<() => Promise<void>>()
      .mockReturnValueOnce(firstAttempt.promise)
      .mockResolvedValueOnce(undefined);
    renderHook(() => useCompanyCatalogPoll({ enabled: true, onDailyRefresh }));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();

    act(() => {
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
      window.dispatchEvent(new Event("focus"));
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();

    await act(async () => {
      firstAttempt.reject(new Error("offline"));
      await firstAttempt.promise.catch(() => undefined);
    });

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
      await Promise.resolve();
    });
    expect(onDailyRefresh).toHaveBeenCalledTimes(2);
    expect(window.localStorage.getItem(LAST_REFRESH_KEY)).toBe("2026-08-17");
  });

  it("cleans up timers and activation listeners when disabled", async () => {
    const onDailyRefresh = vi.fn().mockResolvedValue(undefined);
    const { rerender } = renderHook(
      ({ enabled }) => useCompanyCatalogPoll({ enabled, onDailyRefresh }),
      { initialProps: { enabled: true } },
    );

    rerender({ enabled: false });
    act(() => {
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(24 * 60 * 60 * 1000);
    });
    expect(onDailyRefresh).not.toHaveBeenCalled();
  });

  it("does not persist or reschedule an in-flight refresh after unmount", async () => {
    const attempt = deferredPromise();
    const onDailyRefresh = vi.fn().mockReturnValue(attempt.promise);
    const { unmount } = renderHook(() =>
      useCompanyCatalogPoll({ enabled: true, onDailyRefresh }),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(3000);
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();

    unmount();
    await act(async () => {
      attempt.resolve();
      await attempt.promise;
    });

    expect(window.localStorage.getItem(LAST_REFRESH_KEY)).toBeNull();

    act(() => {
      window.dispatchEvent(new Event("focus"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(24 * 60 * 60 * 1000);
    });
    expect(onDailyRefresh).toHaveBeenCalledOnce();
  });
});

describe("company catalog daily refresh schedule", () => {
  it("runs once after 10:00 when today has not been refreshed", () => {
    const now = new Date(2026, 7, 17, 10, 30, 0);

    expect(companyCatalogRefreshDateKey(now)).toBe("2026-08-17");
    expect(shouldRunCompanyCatalogRefresh(now, "2026-08-16")).toBe(true);
    expect(nextCompanyCatalogRefreshDelayMs(now, "2026-08-16")).toBe(3000);
  });

  it("waits until 10:00 before the daily refresh", () => {
    const now = new Date(2026, 7, 17, 9, 30, 0);

    expect(shouldRunCompanyCatalogRefresh(now, "2026-08-16")).toBe(false);
    expect(nextCompanyCatalogRefreshDelayMs(now, "2026-08-16")).toBe(
      30 * 60 * 1000,
    );
  });

  it("schedules tomorrow after today's refresh succeeded", () => {
    const now = new Date(2026, 7, 17, 10, 30, 0);

    expect(shouldRunCompanyCatalogRefresh(now, "2026-08-17")).toBe(false);
    expect(nextCompanyCatalogRefreshDelayMs(now, "2026-08-17")).toBe(
      23.5 * 60 * 60 * 1000,
    );
  });
});
