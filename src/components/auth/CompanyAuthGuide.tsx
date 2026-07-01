import {
  Bot,
  CheckCircle2,
  MousePointerClick,
  PanelsTopLeft,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import codexProviderEnabledImage from "@/assets/auth-guide/codex-provider-enabled.png";
import codexToolbarEntryImage from "@/assets/auth-guide/codex-toolbar-entry.png";

interface CompanyAuthGuideProps {
  onContinue: () => void;
}

const steps = [
  {
    icon: PanelsTopLeft,
    title: "点击顶部 Codex 图标",
    description:
      "在顶部工具栏找到 Codex 图标，点击后进入 Codex 配置列表。",
  },
  {
    icon: Bot,
    title: "选择公司号池 Codex",
    description:
      "飞书授权后会自动同步公司号池卡片，列表里会显示“公司号池 Codex”。",
  },
  {
    icon: MousePointerClick,
    title: "点击启用并确认使用中",
    description:
      "点击卡片右侧启用，按钮变成“使用中”后即可打开 Codex 使用公司配置。",
  },
];

const guideImages = [
  {
    title: "第一步：点击顶部 Codex 图标",
    description: "箭头指向的位置就是 Codex 入口。",
    src: codexToolbarEntryImage,
    alt: "点击顶部工具栏里的 Codex 图标",
  },
  {
    title: "第二步：启用公司号池 Codex",
    description: "卡片右侧显示“使用中”表示公司号池配置已经生效。",
    src: codexProviderEnabledImage,
    alt: "公司号池 Codex 卡片显示使用中",
  },
];

export function CompanyAuthGuide({ onContinue }: CompanyAuthGuideProps) {
  return (
    <main className="flex min-h-screen items-center justify-center bg-background px-4 py-6">
      <section className="grid w-full max-w-5xl gap-6 rounded-lg border bg-card p-5 shadow-sm md:grid-cols-[minmax(18rem,0.82fr)_minmax(0,1.35fr)]">
        <div className="min-w-0">
          <div className="mb-5 flex items-start gap-3">
            <div className="rounded-md border bg-muted p-2 text-muted-foreground">
              <CheckCircle2 className="h-5 w-5" aria-hidden="true" />
            </div>
            <div>
              <h1 className="text-lg font-semibold text-foreground">
                登录成功，开始使用 CC Switch
              </h1>
              <p className="mt-1 text-sm leading-6 text-muted-foreground">
                完成飞书授权后，按下面步骤启用 Codex 公司号池配置。
              </p>
            </div>
          </div>

          <ol className="space-y-3">
            {steps.map((step, index) => {
              const Icon = step.icon;

              return (
                <li
                  key={step.title}
                  className="flex gap-3 rounded-md border bg-background p-3"
                >
                  <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                    <Icon className="h-4 w-4" aria-hidden="true" />
                  </div>
                  <div className="min-w-0">
                    <div className="text-sm font-medium text-foreground">
                      {index + 1}. {step.title}
                    </div>
                    <p className="mt-1 text-sm leading-5 text-muted-foreground">
                      {step.description}
                    </p>
                  </div>
                </li>
              );
            })}
          </ol>

          <Button type="button" className="mt-5 w-full" onClick={onContinue}>
            我已了解
          </Button>
        </div>

        <div
          className="min-w-0 space-y-3"
          aria-label="Codex 公司号池启用截图说明"
        >
          {guideImages.map((image) => (
            <figure
              key={image.title}
              className="overflow-hidden rounded-md border bg-background"
            >
              <figcaption className="border-b px-3 py-2">
                <div className="text-sm font-medium text-foreground">
                  {image.title}
                </div>
                <div className="mt-0.5 text-xs leading-5 text-muted-foreground">
                  {image.description}
                </div>
              </figcaption>
              <img
                src={image.src}
                alt={image.alt}
                className="block h-auto w-full bg-muted/30"
                loading="eager"
              />
            </figure>
          ))}
        </div>
      </section>
    </main>
  );
}
