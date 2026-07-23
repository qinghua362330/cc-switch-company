import { List, LogOut, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { type CompanyAuthState } from "@/lib/api/companyAuth";
import {
  COMPANY_CATALOG_SELECT_EVENT,
  type CompanyCatalogSelectDetail,
} from "@/components/auth/companyCatalogEvents";

const catalogToolLabel = (tool: string): string => {
  const normalized = tool.toLowerCase();
  if (normalized === "codex") return "Codex";
  if (normalized === "gemini" || normalized === "gemini-cli") return "Gemini";
  if (normalized === "claude" || normalized === "claude-code") {
    return "Claude Code";
  }
  return tool;
};

const catalogTitle = (
  state: Extract<CompanyAuthState, { authenticated: true }>,
) =>
  state.catalog.length > 0
    ? state.catalog
        .map((item) => `${item.label} · ${catalogToolLabel(item.tool)}`)
        .join("\n")
    : "暂无卡片";

const dispatchCatalogSelect = (detail: CompanyCatalogSelectDetail): void => {
  window.dispatchEvent(
    new CustomEvent<CompanyCatalogSelectDetail>(COMPANY_CATALOG_SELECT_EVENT, {
      detail,
    }),
  );
};

interface CompanyAuthShellProps {
  state: Extract<CompanyAuthState, { authenticated: true }>;
  error: string | null;
  isRefreshing: boolean;
  isLoggingOut: boolean;
  onRefresh: () => void;
  onLogout: () => void;
}

export function CompanyAuthShell({
  state,
  error,
  isRefreshing,
  isLoggingOut,
  onRefresh,
  onLogout,
}: CompanyAuthShellProps) {
  const hasCatalog = state.catalog.length > 0;

  return (
    <div
      className="flex min-w-0 max-w-[20rem] shrink items-center gap-1.5 rounded-lg border bg-card/80 px-2 py-1 text-xs shadow-sm lg:max-w-[22rem]"
      aria-label="公司认证状态"
    >
      <div className="hidden min-w-0 flex-1 items-center gap-2 md:flex">
        <div className="min-w-0 flex-[1_1_9rem]">
          <div
            className="truncate font-medium text-foreground"
            title={state.user.displayName}
          >
            {state.user.displayName}
          </div>
          <div
            className="truncate text-muted-foreground"
            title={state.user.email}
          >
            {state.user.email}
          </div>
        </div>
        <div className="h-6 w-px shrink-0 bg-border" />
        <div
          className="flex min-w-0 flex-[0_0_auto] items-center whitespace-nowrap text-muted-foreground"
          title={catalogTitle(state)}
        >
          <span className="shrink-0 font-medium text-foreground">
            {state.catalog.length}
          </span>
          <span className="ml-1 shrink-0">张卡片</span>
        </div>
      </div>
      {error && (
        <span className="hidden max-w-48 truncate text-destructive md:inline">
          {error}
        </span>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="h-8 w-8"
            disabled={!hasCatalog || isLoggingOut}
            title="查看号池卡片"
          >
            <List className="h-4 w-4" aria-hidden="true" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-56">
          {state.catalog.map((item) => (
            <DropdownMenuItem
              key={`${item.tool}:${item.label}:${item.defaultModel}`}
              onSelect={() =>
                dispatchCatalogSelect({
                  tool: item.tool,
                  label: item.label,
                })
              }
            >
              <span className="min-w-0 flex-1 truncate">{item.label}</span>
              <span className="shrink-0 text-xs text-muted-foreground">
                {catalogToolLabel(item.tool)}
              </span>
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        onClick={onRefresh}
        disabled={isRefreshing || isLoggingOut}
        title="刷新目录"
      >
        <RefreshCw
          className={isRefreshing ? "h-4 w-4 animate-spin" : "h-4 w-4"}
          aria-hidden="true"
        />
      </Button>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        onClick={onLogout}
        disabled={isLoggingOut}
        title="退出登录"
      >
        <LogOut className="h-4 w-4" aria-hidden="true" />
      </Button>
    </div>
  );
}
