<div align="center">

# 微行情

**一款轻量、透明、常驻桌面的 Windows 股票行情悬浮窗**

[![Rust](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D4?logo=windows)
![Version](https://img.shields.io/badge/version-0.1.0-2ea44f)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

</div>

微行情使用 Rust 与 `egui` 构建，以无边框悬浮窗展示自选证券的最新价格、涨跌幅和分时走势。程序无需浏览器或 WebView，解压即可运行；支持窗口置顶、背景透明、系统托盘和全局快捷键，适合在工作或学习时低干扰地关注市场行情。

> [!IMPORTANT]
> 本项目仅用于行情展示与技术交流，不提供交易功能，也不构成任何投资建议。第三方公开行情接口可能存在延迟、错误或不可用，请勿将其作为交易决策的唯一依据。

## 主要功能

- **桌面悬浮显示**：无边框、可拖动、可置顶，背景支持 0%–100% 不透明度，文字始终保持清晰。
- **覆盖常用市场**：支持沪深股票、指数、ETF 与港股，最多可添加 50 只证券并混合排序。
- **行情自动容错**：优先连接新浪 WebSocket 推送，异常时自动降级至腾讯或新浪 HTTP，并按退避策略重连。
- **分时走势图**：展示最近交易日分钟价格、涨跌幅与昨收参考线，支持鼠标悬停查看分钟数据。
- **快捷搜索自选**：支持名称、部分名称、拼音首字母及证券代码搜索，例如“茅台”“腾讯”、`gzmt`、`600519`、`00700`。
- **托盘与快捷键**：隐藏后常驻系统托盘，默认使用 `Ctrl+Alt+H` 在任意应用中显示或隐藏窗口。
- **个性化设置**：支持紧凑模式、行情刷新模式、刷新间隔、窗口位置和全局快捷键配置。
- **在线自动更新**：启动时或托盘菜单一键通过 GitHub Releases 检查新版本，支持下载进度展示、加速镜像配置及一键无缝覆盖安装。
- **本地保存配置**：自选列表和偏好设置保存在本机，损坏配置会先备份再恢复默认值。

## 快速开始

### 下载与安装

1. 打开项目的 [Releases 页面](https://github.com/xxofficial/mini_stock_monitoring/releases)。
2. 下载并运行 `微行情-<版本号>-windows-x64-setup.exe`。
3. 按安装向导操作；“创建桌面快捷方式”默认勾选，也可以按需取消。

当前版本面向 **Windows 10/11 x64**，显卡驱动需要支持 OpenGL 3.3。安装程序会创建开始菜单快捷方式，并可选创建桌面快捷方式；卸载入口位于 Windows 的“已安装的应用”。如需免安装使用，也可以下载 `MiniStockMonitor-windows-x64.zip`，解压后双击 `微行情.exe`。

首次启动默认显示上证指数、贵州茅台和平安银行。若 Releases 暂无可下载文件，也可以按照下方说明从源码运行。

### 从源码运行

请先安装 [Rust](https://www.rust-lang.org/tools/install) 1.95+、MSVC 工具链以及 Windows SDK / Visual Studio C++ Build Tools。

```powershell
git clone https://github.com/xxofficial/mini_stock_monitoring.git
cd mini_stock_monitoring
cargo run --locked
```

## 使用说明

| 操作 | 使用方式 |
| --- | --- |
| 移动窗口 | 拖动标题、行情行或窗口空白区域 |
| 添加自选 | 点击“添加自选”，输入名称、拼音首字母或代码 |
| 排序 / 删除 | 右键行情行，选择上移、下移或移除 |
| 查看分时 | 点击行情行，或右键选择“查看分时走势” |
| 返回列表 | 在分时页点击“自选列表”，或按 `Esc` |
| 显示 / 隐藏 | 按 `Ctrl+Alt+H`，或左键单击托盘图标 |
| 打开设置 | 点击齿轮按钮，或按 `Ctrl+,` |
| 手动刷新 | 点击刷新按钮，或按 `Ctrl+R` |
| 检查更新 | 托盘菜单“检查更新”，或在设置面板点击“检查更新” |
| 隐藏到托盘 | 点击右上角横线按钮 |
| 退出程序 | 点击右上角关闭按钮、托盘菜单“退出”，或按 `Ctrl+Q` |

全局快捷键可在设置中重新录制、停用或恢复默认值。窗口高度会随自选数量调整，超过 7 行后自动滚动；开启“紧凑显示”可进一步缩小占用空间。

## 支持的证券与代码

| 类型 | 输入示例 | 说明 |
| --- | --- | --- |
| 上海股票 / ETF | `600519`、`sh600519`、`600519.SH` | 以 5、6、9 开头的六位代码默认识别为上海市场 |
| 深圳股票 / ETF | `000001`、`sz000001` | 以 0、1、2、3 开头的六位代码默认识别为深圳市场 |
| 指数 | `sh000001` 或搜索“上证指数” | 纯数字 `000001` 默认表示深圳的平安银行 |
| 港股 | `00700`、`hk700`、`700.HK` | 自动补齐为五位港股代码 |

港股价格与涨跌额显示三位小数，价格和成交额保持证券原始交易币种，不进行汇率换算。

暂不支持北交所、美股、基金净值或交易下单。

## 行情与分时数据

- 自动模式使用新浪 WebSocket 接收沪深及港股行情；连接失败时自动使用腾讯 HTTP，随后尝试新浪 HTTP。
- 也可以在设置中切换为腾讯定时刷新，刷新间隔可设为 2–60 秒。
- 分时图使用腾讯分钟数据，盘中约每 15 秒刷新；午休和休市期间自动降低请求频率。
- 沪深时间轴为 09:30–11:30、13:00–15:00；港股时间轴为 09:30–12:00、13:00–16:10。
- 断网时会保留最后一次有效行情或走势，恢复连接后自动更新；较旧的快照不会覆盖较新的数据。
- 非交易日显示接口提供的最近交易日数据。目前未接入交易所节假日和半日市日历。

项目使用的是网页公开行情接口，其格式和访问条件可能随时变化。“推送已连接”仅表示网络通道可用，不代表数据无延迟或市场正在交易。

## 配置文件

配置默认保存在：

```text
%APPDATA%\MiniStockMonitor\settings.json
```

其中包含自选列表、透明度、置顶状态、紧凑模式、行情模式、刷新间隔、窗口位置和显示 / 隐藏快捷键。保存时会先写入临时文件再替换原文件，避免意外中断损坏配置；若发现配置格式异常，程序会将原文件备份为 `settings.invalid-*.json`。

开发和测试时，可使用 `MINI_STOCK_CONFIG_DIR` 指定独立配置目录。

## 构建、测试与打包

```powershell
# 运行测试
cargo test --locked --all-targets

# 静态检查
cargo clippy --locked --all-targets -- -D warnings

# 构建发布版本
cargo build --locked --release --bin mini-stock-monitor

# 生成 Windows 安装程序、便携版 ZIP 和 SHA-256 校验文件
# 需要预先安装 Inno Setup 6
powershell -ExecutionPolicy Bypass -File scripts/package.ps1
```

生成的发布文件位于：

```text
dist/MiniStockMonitor/
dist/MiniStockMonitor/LICENSE
dist/微行情-0.1.0-windows-x64-setup.exe
dist/MiniStockMonitor-windows-x64.zip
dist/SHA256SUMS.txt
```

如需验证真实网络行情链路，可以运行：

```powershell
cargo run --locked --bin quote-check
cargo run --locked --bin quote-check -- sh000001 sh600519 sz000001 hk00700
```

## 技术实现

| 模块 | 技术 / 职责 |
| --- | --- |
| 桌面界面 | Rust、`eframe/egui`、Glow / OpenGL |
| 异步网络 | Tokio、Reqwest、Tokio Tungstenite |
| 实时行情 | 新浪 WebSocket，腾讯 / 新浪 HTTP 降级 |
| 分时数据 | 腾讯分钟行情接口 |
| Windows 集成 | 单实例、系统托盘、全局快捷键、屏幕位置校正 |
| 配置持久化 | Serde JSON、临时文件原子替换 |

网络任务运行在独立线程的 Tokio 运行时中，界面与行情线程仅共享有界的最新状态；只有收到数据或发生用户操作时才触发重绘，避免持续高帧率占用资源。

## 项目结构

```text
src/
├─ main.rs                 窗口初始化与应用入口
├─ ui.rs                   悬浮窗、自选列表与设置界面
├─ ui/intraday_chart.rs    分时图绘制与悬停读数
├─ feed.rs                 推送、心跳、重连与 HTTP 降级
├─ quote.rs                证券代码、行情模型与数据解析
├─ search.rs               名称 / 拼音搜索与请求取消
├─ intraday.rs             分钟数据、交易时段与分时缓存
├─ config.rs               配置读取、校验与原子保存
├─ tray.rs                 Windows 系统托盘
├─ hotkey.rs               快捷键解析与校验
├─ global_hotkey.rs        Windows 全局快捷键注册
├─ platform.rs             单实例与平台相关能力
└─ bin/quote_check.rs      真实行情链路诊断工具
```

## 参与贡献

欢迎提交 [Issue](https://github.com/xxofficial/mini_stock_monitoring/issues) 或 Pull Request。提交代码前，请确保以下检查能够通过：

```powershell
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

如果问题与行情接口有关，请尽量附上证券代码、发生时间、当前网络环境和界面提示，但不要上传包含个人信息的配置文件。

## 开源许可

本项目基于 [MIT License](LICENSE) 开源。你可以自由使用、复制、修改、合并、发布和分发本项目，但需要保留原始版权声明和许可声明。

---

如果这个小工具对你有帮助，欢迎点一个 Star，让更多人发现它。
