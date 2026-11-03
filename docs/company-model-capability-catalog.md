# 公司模型能力目录：来源与发布验收

## 解析顺序

公司号池 / 第三方 GPT 的自动能力按以下顺序解析：

1. 本机选定 Codex 客户端的 `debug models --bundled`。不需要登录，不读取账号 `models_cache.json`，也不读取 CC Switch 自己生成的目录。
2. 本机没有匹配模型时，读取公司下发的 `capabilities.catalog_snapshot`。原生行必须满足 schema 1、模型名匹配和 **Codex 完整版本字符串完全相同**；否则不用该原生行，仍可使用经过校验的通用能力字段。
3. 服务端快照由相应 Codex 发布版本的 bundled JSON 和 models.dev 离线 JSON 生成。bundled 字段优先；models.dev 只补缺失通用字段，不生成提示词、工具契约、`comp_hash` 或推理档位。
4. 仍无有效上下文、也没有有效显式窗口时，回退 128000。

有效显式 `contextWindow` / TOML 窗口配置覆盖自动值。自动 `capabilities` 不转成显式窗口；公司刷新保留仍存在模型的显式字段，但替换旧快照，不复活已下架模型。原生 prompt/tools 保持既有镜像规则；公司路由不复制仅适用官方后端的账号和协议字段。

官方登录账号模型继续走已有的账号目录优先、bundled 兜底路径，不受公司模型快照覆盖。

## 本机客户端选择

- `CC_SWITCH_CODEX_CLI` 显式路径优先。
- macOS 检测到唯一运行中的 ChatGPT/Codex bundle 时，优先其内置 CLI（只读进程可执行路径，不读启动参数）。
- 否则用已安装桌面客户端，再用独立 CLI。多个运行 bundle 不猜测；可用上述环境变量明确选择。
- Windows/Linux 依赖显式路径或安装位置启发式；不声称自动识别所有运行实例。
- 选中客户端导出失败时，不借用另一个旧 CLI 的目录。允许服务端兜底；临时失败下次会重试。
- 缓存按可执行文件路径、解析后的真实路径、文件时间/大小和版本失效，客户端升级不会永久沿用进程第一次加载的旧目录。

环境变量应设置在启动 CC Switch 的进程环境中；仅在终端设置不会改变已运行 GUI 进程的环境。

## 服务端快照契约

```json
{
  "context_window": 500000,
  "catalog_snapshot": {
    "schema_version": 1,
    "codex_version": "<exact-client-version>",
    "model": { "slug": "<exact-model-id>", "context_window": 500000 },
    "source": { "kind": "codex-bundled", "sha256": "<input-sha256>" }
  }
}
```

上面仅示意结构，`model` 实际必须是完整的原生行，不能用这个片段充当目录。哈希记录输入来源，不等于供应商签名。构建者负责确保版本标签对应真实发布文件。

当前格式每个模型携带一个原生 build，不实现按客户端版本进行 HTTP 多版本协商。不同 build 客户端拒用不兼容原生行，但仍可使用通用上下文；需要完整原生工具契约时，发布匹配 build 的快照。

服务端实现和构建说明位于 catalog 服务仓库 `services/provision/README.md`。必须将构建产物放到 `PROVISION_MODEL_CAPABILITIES_FILE` 指定的持久化文件；只更新代码不会凭空生成生产能力文件。坏文件保留最近一次有效数据；没有文件表示没有服务端补全。

## 发布后验收

1. 记录实际 CC Switch 安装版本、所选 Codex 路径及 `--version`。
2. 导出该客户端 `debug models --bundled`；按模型逐项核对 `context_window`、`max_context_window`、压缩阈值、推理档位和原生工具字段，不能把所有模型硬设成同一窗口。
3. 核对服务端 catalog 响应的模型 key、版本、输入来源哈希和完整快照；不要在日志里输出 API key / token。
4. 刷新公司目录，进入并保存模型编辑页，再检查生成目录；快照应保留，自动窗口不应变成用户显式覆盖。
5. 本机命中时用本机值；不命中且版本一致时用服务端原生行；版本不同时仅用通用字段；完全缺失时为 128000。显式窗口始终优先。
6. 先做小型新会话冒烟，再验证旧会话。旧会话容量报错、模型切换压缩和远端网关故障仍需请求日志定位；正确目录本身不证明所有卡住问题均消失。

源码和单元测试通过不表示运行中的安装包或生产能力文件已更新。发布/安装必须单独执行并按上述步骤验证。
