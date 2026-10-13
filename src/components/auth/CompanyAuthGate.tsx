import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";
import { useQueryClient } from "@tanstack/react-query";
import { CompanyAuthGuide } from "@/components/auth/CompanyAuthGuide";
import { CompanyAuthLogin } from "@/components/auth/CompanyAuthLogin";
import { CompanyAuthShell } from "@/components/auth/CompanyAuthShell";
import { useCompanyCatalogPoll } from "@/hooks/useCompanyCatalogPoll";
import { providersApi, settingsApi } from "@/lib/api";
import {
  getCompanyAuthState,
  loginWithCompanyTicket,
  logoutCompanyAuth,
  refreshCompanyCatalog,
  startCompanyFeishuLogin,
  syncCompanyProviders,
  toCompanyAuthError,
  type CompanyAuthState,
} from "@/lib/api/companyAuth";
import { COMPANY_AUTH_LOGIN_EVENT } from "@/components/auth/companyCatalogEvents";
import { showCompanyCatalogUpdatedDialog } from "@/lib/userAttention";

interface CompanyAuthGateProps {
  children: (props: { shell: ReactNode }) => ReactNode;
}

const GUIDE_ACK_STORAGE_KEY = "token-switch-company-auth-guide-ack-v1";

const readGuideAcknowledged = (): boolean => {
  try {
    return window.localStorage.getItem(GUIDE_ACK_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
};

const writeGuideAcknowledged = (): void => {
  try {
    window.localStorage.setItem(GUIDE_ACK_STORAGE_KEY, "1");
  } catch {
    // Ignore storage failures; the button should still allow entry.
  }
};

export function CompanyAuthGate({ children }: CompanyAuthGateProps) {
  const queryClient = useQueryClient();
  const [state, setState] = useState<CompanyAuthState | null>(null);
  const [ticket, setTicket] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [isLoadingState, setIsLoadingState] = useState(true);
  const [isLoggingIn, setIsLoggingIn] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [isLoggingOut, setIsLoggingOut] = useState(false);
  const [hasAcknowledgedGuide, setHasAcknowledgedGuide] = useState(
    readGuideAcknowledged,
  );

  const stateRef = useRef<CompanyAuthState | null>(null);
  const pendingSyncRef = useRef(true);
  const queueRef = useRef<Promise<void>>(Promise.resolve());
  const generationRef = useRef(0);

  // Include sync in the transaction: the Rust sync command reads the stored catalog.
  // An intervening refresh must not change that catalog before sync completes.
  const enqueue = (operation: () => Promise<void>): Promise<void> => {
    const result = queueRef.current.then(operation);
    queueRef.current = result.catch(() => {});
    return result;
  };
  const isCurrent = (generation: number) =>
    generationRef.current === generation;
  const ensureCurrent = (generation: number) => {
    if (!isCurrent(generation))
      throw new Error("Company authentication changed");
  };
  const publishState = (nextState: CompanyAuthState) => {
    stateRef.current = nextState;
    setState(nextState);
  };
  const syncCompanyProviderCards = async (generation: number) => {
    ensureCurrent(generation);
    await syncCompanyProviders();
    ensureCurrent(generation);
    await queryClient.invalidateQueries({ queryKey: ["providers"] });
    ensureCurrent(generation);
    await providersApi.updateTrayMenu();
    ensureCurrent(generation);
    pendingSyncRef.current = false;
  };
  const handleAuthError = async (cause: unknown, generation: number) => {
    if (!isCurrent(generation)) return;
    const authError = toCompanyAuthError(cause);
    setError(authError.message);
    if (
      authError.kind === "session_expired" ||
      authError.kind === "invalid_ticket"
    ) {
      // Close the gate even if credential cleanup fails. Never clear user configs.
      generationRef.current += 1;
      publishState({ authenticated: false });
      pendingSyncRef.current = true;
      setTicket("");
      await logoutCompanyAuth().catch(() => {});
    }
  };

  // 每天 10:00 拉取线上目录；仅在同步成功后确认 metadata，失败可重试。
  const refreshCatalog = (daily: boolean): Promise<void> => {
    const generation = generationRef.current;
    return enqueue(async () => {
      ensureCurrent(generation);
      const previous = stateRef.current;
      if (!previous?.authenticated)
        throw new Error("Company authentication changed");
      try {
        const refreshed = await refreshCompanyCatalog();
        ensureCurrent(generation);
        const nextState: CompanyAuthState = {
          authenticated: true,
          user: previous.user,
          catalog: refreshed.catalog,
          baseUrl: refreshed.baseUrl ?? previous.baseUrl,
        };
        const hasChanged =
          nextState.baseUrl !== previous.baseUrl ||
          JSON.stringify(nextState.catalog) !==
            JSON.stringify(previous.catalog);
        if (!daily || hasChanged || pendingSyncRef.current) {
          pendingSyncRef.current = true;
          await syncCompanyProviderCards(generation);
        }
        ensureCurrent(generation);
        publishState(nextState);
        // A queued retry must not hide the preceding failure before it succeeds.
        setError(null);
        if (daily && hasChanged) await showCompanyCatalogUpdatedDialog();
        ensureCurrent(generation);
      } catch (cause) {
        await handleAuthError(cause, generation);
        throw cause; // Poll only records today after the entire transaction succeeds.
      }
    });
  };

  useCompanyCatalogPoll({
    enabled: state?.authenticated === true,
    onDailyRefresh: () => refreshCatalog(true),
  });

  useEffect(() => {
    const generation = generationRef.current;
    void enqueue(async () => {
      if (!isCurrent(generation)) return;
      setIsLoadingState(true);
      try {
        const nextState = await getCompanyAuthState();
        ensureCurrent(generation);
        publishState(nextState);
        setError(null);
        if (nextState.authenticated) {
          pendingSyncRef.current = true;
          await syncCompanyProviderCards(generation);
        }
      } catch (cause) {
        await handleAuthError(cause, generation);
        if (isCurrent(generation) && stateRef.current === null) {
          publishState({ authenticated: false });
        }
      } finally {
        setIsLoadingState(false);
      }
    });
    return () => {
      generationRef.current += 1;
    };
  }, []);

  const refresh = async () => {
    setIsRefreshing(true);
    try {
      await refreshCatalog(false);
    } catch {
      /* refreshCatalog reports a sanitized error. */
    } finally {
      setIsRefreshing(false);
    }
  };

  const logout = async () => {
    const previous = stateRef.current;
    const generation = ++generationRef.current;
    setIsLoggingOut(true);
    setError(null);
    publishState({ authenticated: false });
    pendingSyncRef.current = true;
    setTicket("");
    // Always run cleanup, even if a new login was queued while logout waited.
    await enqueue(async () => {
      try {
        await logoutCompanyAuth();
      } catch {
        if (isCurrent(generation)) {
          if (previous) publishState(previous);
          setError("退出登录失败，请稍后重试。");
        }
      } finally {
        setIsLoggingOut(false);
      }
    });
  };

  const shell = state?.authenticated ? (
    <CompanyAuthShell
      state={state}
      error={error}
      isRefreshing={isRefreshing}
      isLoggingOut={isLoggingOut}
      onRefresh={() => void refresh()}
      onLogout={() => void logout()}
    />
  ) : null;

  const openFeishu = async () => {
    setError(null);
    try {
      const start = await startCompanyFeishuLogin();
      await settingsApi.openExternal(start.url);
    } catch (openError) {
      setError(toCompanyAuthError(openError).message);
    }
  };

  const submitTicket = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const generation = ++generationRef.current;
    const submittedTicket = ticket;
    setError(null);
    setIsLoggingIn(true);
    await enqueue(async () => {
      if (!isCurrent(generation)) return;
      try {
        const nextState = await loginWithCompanyTicket(submittedTicket);
        ensureCurrent(generation);
        publishState(nextState);
        pendingSyncRef.current = true;
        setTicket("");
        // No credential data crosses this event boundary.
        window.dispatchEvent(new Event(COMPANY_AUTH_LOGIN_EVENT));
        await syncCompanyProviderCards(generation);
      } catch (cause) {
        await handleAuthError(cause, generation);
      } finally {
        setIsLoggingIn(false);
      }
    });
  };

  if (isLoadingState || state === null) {
    return (
      <div className="flex min-h-screen items-center justify-center bg-background text-sm text-muted-foreground">
        正在检查登录状态...
      </div>
    );
  }

  if (!state.authenticated) {
    return (
      <CompanyAuthLogin
        ticket={ticket}
        error={error}
        isLoggingIn={isLoggingIn}
        onTicketChange={setTicket}
        onOpenFeishu={() => void openFeishu()}
        onSubmitTicket={(event) => void submitTicket(event)}
      />
    );
  }

  if (!hasAcknowledgedGuide) {
    return (
      <CompanyAuthGuide
        onContinue={() => {
          writeGuideAcknowledged();
          setHasAcknowledgedGuide(true);
        }}
      />
    );
  }

  return <>{children({ shell })}</>;
}
