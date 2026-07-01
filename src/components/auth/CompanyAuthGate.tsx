import {
  type FormEvent,
  type ReactNode,
  useEffect,
  useMemo,
  useState,
} from "react";
import { useQueryClient } from "@tanstack/react-query";
import { CompanyAuthGuide } from "@/components/auth/CompanyAuthGuide";
import { CompanyAuthLogin } from "@/components/auth/CompanyAuthLogin";
import { CompanyAuthShell } from "@/components/auth/CompanyAuthShell";
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

  const syncCompanyProviderCards = async (nextState: CompanyAuthState) => {
    if (!nextState.authenticated) return;

    try {
      await syncCompanyProviders();
      await queryClient.invalidateQueries({ queryKey: ["providers"] });
      await providersApi.updateTrayMenu();
    } catch (syncError) {
      console.warn("[CompanyAuth] Failed to sync company providers", syncError);
    }
  };

  useEffect(() => {
    let active = true;

    const load = async () => {
      setIsLoadingState(true);
      try {
        const nextState = await getCompanyAuthState();
        if (active) {
          setState(nextState);
          setError(null);
          void syncCompanyProviderCards(nextState);
        }
      } catch (loadError) {
        if (active) {
          setState({ authenticated: false });
          setError(toCompanyAuthError(loadError).message);
        }
      } finally {
        if (active) setIsLoadingState(false);
      }
    };

    void load();
    return () => {
      active = false;
    };
  }, []);

  const shell = useMemo(() => {
    if (!state?.authenticated) return null;

    const refresh = async () => {
      setIsRefreshing(true);
      setError(null);
      try {
        const refreshed = await refreshCompanyCatalog();
        setState({
          authenticated: true,
          user: state.user,
          catalog: refreshed.catalog,
          baseUrl: refreshed.baseUrl ?? state.baseUrl,
        });
        void syncCompanyProviderCards({
          authenticated: true,
          user: state.user,
          catalog: refreshed.catalog,
          baseUrl: refreshed.baseUrl ?? state.baseUrl,
        });
      } catch (refreshError) {
        const authError = toCompanyAuthError(refreshError);
        if (authError.kind === "invalid_ticket") {
          await logoutCompanyAuth();
          setState({ authenticated: false });
          setTicket("");
        }
        setError(authError.message);
      } finally {
        setIsRefreshing(false);
      }
    };

    const logout = async () => {
      setIsLoggingOut(true);
      setError(null);
      try {
        await logoutCompanyAuth();
        setState({ authenticated: false });
        setTicket("");
      } catch {
        setError("退出登录失败，请稍后重试。");
      } finally {
        setIsLoggingOut(false);
      }
    };

    return (
      <CompanyAuthShell
        state={state}
        error={error}
        isRefreshing={isRefreshing}
        isLoggingOut={isLoggingOut}
        onRefresh={() => void refresh()}
        onLogout={() => void logout()}
      />
    );
  }, [error, isLoggingOut, isRefreshing, state]);

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
    setError(null);
    setIsLoggingIn(true);
    try {
      const nextState = await loginWithCompanyTicket(ticket);
      setState(nextState);
      void syncCompanyProviderCards(nextState);
      setTicket("");
    } catch (loginError) {
      setState({ authenticated: false });
      setError(toCompanyAuthError(loginError).message);
    } finally {
      setIsLoggingIn(false);
    }
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
