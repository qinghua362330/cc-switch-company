import { type FormEvent } from "react";
import { ShieldCheck, Ticket, UserRound } from "lucide-react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

interface CompanyAuthLoginProps {
  ticket: string;
  error: string | null;
  isLoggingIn: boolean;
  onTicketChange: (ticket: string) => void;
  onOpenFeishu: () => void;
  onSubmitTicket: (event: FormEvent<HTMLFormElement>) => void;
}

export function CompanyAuthLogin({
  ticket,
  error,
  isLoggingIn,
  onTicketChange,
  onOpenFeishu,
  onSubmitTicket,
}: CompanyAuthLoginProps) {
  return (
    <main className="flex min-h-screen items-center justify-center bg-background px-4">
      <section className="w-full max-w-md rounded-lg border bg-card p-6 shadow-sm">
        <div className="mb-6 flex items-start gap-3">
          <div className="rounded-md border bg-muted p-2 text-muted-foreground">
            <ShieldCheck className="h-5 w-5" aria-hidden="true" />
          </div>
          <div>
            <h1 className="text-lg font-semibold text-foreground">
              公司账号登录
            </h1>
            <p className="mt-1 text-sm text-muted-foreground">
              使用飞书认证后进入 cc-switch。
            </p>
          </div>
        </div>

        <div className="space-y-4">
          <Button
            type="button"
            className="w-full"
            onClick={onOpenFeishu}
            disabled={isLoggingIn}
          >
            <UserRound className="mr-2 h-4 w-4" aria-hidden="true" />
            使用飞书登录
          </Button>

          <form className="space-y-3" onSubmit={onSubmitTicket}>
            <div className="space-y-2">
              <Label htmlFor="feishu-ticket">一次性 ticket</Label>
              <div className="relative">
                <Ticket
                  className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
                  aria-hidden="true"
                />
                <Input
                  id="feishu-ticket"
                  value={ticket}
                  onChange={(event) => onTicketChange(event.target.value)}
                  placeholder="fs_..."
                  className="pl-9"
                  autoComplete="one-time-code"
                  disabled={isLoggingIn}
                />
              </div>
            </div>

            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}

            <Button
              type="submit"
              variant="outline"
              className="w-full"
              disabled={isLoggingIn}
            >
              {isLoggingIn ? "正在登录..." : "提交 ticket"}
            </Button>
          </form>
        </div>
      </section>
    </main>
  );
}
