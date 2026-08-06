# CaPilot ESP32-C5 固件架构设计

> **版本:** v0.1
> **日期:** 2026-08-05
> **作者:** HaChi + Claude
> **关联:**
> - [CaPilot-PRD.md](CaPilot-PRD.md) v3.0（产品需求，尤其 H1–H6）
> - [CaPilot-IDE-DevPlan.md](CaPilot-IDE-DevPlan.md) §8（ESP 硬件集成）与 §9（语音链路）
> - IDE 侧协议实现 `/home/hachi/Project/CaPilot-Ide/src-tauri/src/esp/protocol.rs`（帧编解码权威）
> **状态:** Draft — 作为 ESP 固件实现指南

---

## 目录

1. [概述](#1-概述)
2. [现状分析与差距](#2-现状分析与差距)
3. [总体架构](#3-总体架构)
4. [目录 / 模块结构](#4-目录--模块结构)
5. [板级抽象层（board）](#5-板级抽象层board)
6. [传输层（ble / usb / wifi）](#6-传输层ble--usb--wifi)
7. [帧协议（proto）](#7-帧协议proto)
8. [音频管线（audio）](#8-音频管线audio)
9. [应用状态机（app）](#9-应用状态机app)
10. [UI 设计（ui / LVGL）](#10-ui-设计ui--lvgl)
11. [设置与持久化](#11-设置与持久化)
12. [里程碑与任务拆分](#12-里程碑与任务拆分)
13. [风险与待验证](#13-风险与待验证)
14. [兼容性与迁移](#14-兼容性与迁移)

---

## 1. 概述

### 1.1 目标

本文档为 CaPilot 的 ESP32-C5 遥控器固件（`Firmware/PlatformIO/`）提供可直接落地的实现设计，覆盖：

1. **模块化固件架构**：在现有单文件 `main.cpp` 的 BLE NUS 遥测固件基础上，演进出支持 PRD 三模式的工程结构。
2. **帧协议**：与 IDE 侧 `protocol.rs` 完全兼容的 `CA 50 + type + len + seq + version + payload + crc16` 帧协议，定义全部消息类型、载荷 schema、seq/ack/重试语义。
3. **音频管线**：ESP 麦克风 → Opus → BLE 上行；回复文本帧下行 → ESP 侧 TTS 播报；barge-in（PTT 打断）。
4. **应用状态机**：boot → idle → connecting → connected → 模式内子状态，以及触摸 / PTT 触发的模式切换。
5. **里程碑**：与 IDE 开发计划 P3 平行的固件阶段拆分。

### 1.2 产品形态（摘自 PRD v3.0）

```
┌─────────────────────────────────────────────┐
│  CaPilot IDE（电脑端，Tauri）               │
│  ├─ Agent 编排核心 + master/worker 模型      │
│  ├─ sherpa-onnx 流式 STT（本地，中文）       │
│  └─ EspTransport（BLE-NUS 默认 / USB / WiFi）│
└──────────────────┬──────────────────────────┘
                   │ BLE NUS 数据通道（默认优先）
┌──────────────────▼──────────────────────────┐
│  ESP32-C5 遥控器（240×320 竖屏，1×BOOT）     │
│  列表模式（4 卡片/页 + 翻页 + 长按菜单）      │
│  设置模式（与 IDE 双向同步）                 │
│  master 语音模式（PTT + 字幕 + 喇叭播报）     │
└─────────────────────────────────────────────┘
```

### 1.3 硬件平台（Waveshare C5 Touch LCD 2.8"）

| 外设 | 型号 / 接口 | 关键引脚 |
| --- | --- | --- |
| 显示屏 | ST7789 240×320 SPI | MOSI=7, SCLK=6, CS=10, DC=9, RST=EXIO1 |
| 触摸 | CST3530 I²C 0x58 | SDA=0, SCL=1, INT=5 |
| IO 扩展 | CH32V003 I²C 0x24 | TP_RST=EXIO0, LCD_RST=EXIO1, PA/背光 PWM=EXIO3 |
| 音频 Codec | ES8311（I²C，ADC+DAC） | 引脚待从原理图确认（见 §5.3） |
| 按键 | BOOT0（GPIO28，按下拉低） | 唯一软件可用键 |
| 电池 | ADC1_CH1 (GPIO2) | 分压采样 |
| BLE | ESP32-C5 BLE 5.0（NimBLE） | — |
| 内存 | **无 PSRAM**（当前 board 配置），单核 RISC-V | SRAM ~400KB 可用 |

> **注意**：PRD §6.5 声称 C5 有 8MB PSRAM，但当前 `board.h` 与 `platformio.ini` 均标注 `BOARD_HAS_PSRAM 0` / "No PSRAM"。本设计按**无 PSRAM、单核、~400KB SRAM** 为硬约束；若后续实机确认有 PSRAM，可放宽 LVGL 缓冲与音频缓冲设计（代码路径在 §4 预留）。

### 1.4 术语

| 术语 | 含义 |
| --- | --- |
| 帧 (frame) | BLE 数据通道上传输的最小协议单元，`magic|type|len|seq|version|payload|crc16` |
| 消息 (message) | 一个业务载荷（JSON），可跨多个帧分段 |
| 可靠帧 | 需要 Ack 确认、失败重试的帧（控制类） |
| fire-and-forget | 不确认、不重试的帧（音频、心跳、遥测） |
| PTT | Push-to-Talk，长按 BOOT 说话、松开发送 |
| barge-in | 播报中再次长按 PTT 立即打断播报 |

---

## 2. 现状分析与差距

### 2.1 现有固件（`Firmware/PlatformIO/src/main.cpp`）

已实现且**保留**的部分：

- NimBLE 外设模式，广播名 `CaPilot-C5`，NUS 服务（6E400001-B5A3-F393-E0A9-E50E24DCCA9E）
- NUS RX 写（IDE→ESP）回调，收到写后回 `Ack` 帧
- NUS TX Notify（ESP→IDE）遥测：2s 一帧 JSON `{batt_mv,batt_pct,uptime_s,heap_free,status}`
- CRC-16/CCITT (poly 0x1021, init 0xFFFF)，与 IDE `protocol.rs` 算法一致
- 断线后重启广播

### 2.2 关键差距（设计必须处理）

| # | 差距 | 影响 | 处理 |
| --- | --- | --- | --- |
| G1 | **帧头缺 `seq + version`**。现有帧布局为 `magic(2)+type(1)+len(2)+payload+crc(2)`；IDE 的 `try_decode()` 要求 9 字节头，把 payload 首 2 字节误读为 seq/version，**当前固件的帧在 IDE 端无法解码** | 遥测/控制全部不兼容 | §7 重写帧布局，与 `protocol.rs` 完全一致 |
| G2 | 无应用级消息类型（只有 telemetry/ack） | 无法承载 agent_update / 触摸事件等 | §7.4 消息目录 |
| G3 | 无 seq/ack/重试、无心跳超时 | BLE 不可靠管道下控制帧会丢 | §7.6 |
| G4 | 无 UI / 触摸 / 模式 | PRD 三模式未实现 | §9 / §10 |
| G5 | 无音频采集 / 编码 / 播放 | master 语音未实现 | §8 |
| G6 | 无 MTU 协商 | 默认 23 字节 MTU，吞吐低 | §6.3 |
| G7 | 无设置持久化 | 重启丢配置 | §11（NVS） |

### 2.3 需新增依赖

| 依赖 | 用途 | 备注 |
| --- | --- | --- |
| `espressif/opus`（ESP-IDF 托管组件） | Opus 编码/解码 | DevPlan §9 指定 libopus；PlatformIO `lib_deps` 引入 |
| `espressif/esp-sr`（可选） | 板载中文 TTS（esp-tts） | 需验证 C5 支持；flash 中放音色数据；若不支持则走 IDE 侧 TTS 音频下行（§8.4） |
| 现有 | `lvgl@9.2.0` / `ArduinoJson@7` / `NimBLE-Arduino@2.1.1` / `GFX Library` | 保持 |

---

## 3. 总体架构

### 3.1 架构分层

```
┌────────────────────────────────────────────────────────────────┐
│  app/     应用状态机 · 三模式编排 · 与 IDE 的会话协调            │
├────────────────────────────────────────────────────────────────┤
│  ui/      LVGL 屏幕（list / settings / master）· 触摸 · 手势    │
├───────────────────────────────┬────────────────────────────────┤
│  audio/   采集→Opus 编码       │  ble/      传输链接抽象         │
│           TTS 解码→播放        │  (NUS 默认 / USB / WiFi 预留)   │
│           PTT · barge-in       │  proto/    帧编解码 · seq/ack   │
│                               │           分段 · 心跳           │
├───────────────────────────────┴────────────────────────────────┤
│  board/   板级抽象（引脚/外设/LVGL flush/触摸/背光/电池）        │
├────────────────────────────────────────────────────────────────┤
│  sys/     任务编排 · NVS 设置 · 遥测（电池/堆/固件版本）         │
└────────────────────────────────────────────────────────────────┘
           ↓ 全部跑在 ESP32-C5（单核 RISC-V，FreeRTOS 任务）
```

### 3.2 任务 / 线程模型

C5 为**单核**，因此"并行"实际是 FreeRTOS 优先级抢占。设计要点：I2S 走 DMA + 中断（采集不占 CPU），LVGL 用 `lv_timer` 周期性刷新，协议帧在 BLE 回调里只做入队、不在中断上下文解码。

| 任务 | 优先级 | 周期/触发 | 职责 | 关键限制 |
| --- | --- | --- | --- | --- |
| `app_task` | 5 | 事件驱动 | 状态机推进、模式切换、消息路由 | 不得阻塞 >10ms |
| `audio_task` | 7（最高） | 20ms（对齐 Opus 帧） | I2S 读取→Opus 编码→入 TX 队列；TTS 解码→I2S 写 | 实时性最强，优先级最高 |
| `ble_tx_task` | 6 | 队列有帧 | 从 TX 队列取帧→`notify()`；控制帧重试计时 | 每帧 ≤ MTU |
| `ui_task` | 4 | `lv_timer` 5ms tick | LVGL 渲染、触摸轮询、手势判定 | 不与 audio 抢 CPU |
| `telemetry_task` | 3 | 2s | 电池 ADC / heap / 心跳 | 心跳 5s |
| `button_task` | 4 | 10ms 轮询 | BOOT 消抖、短按/长按判定 | 长按阈值 500–700ms |

> 单核下 audio 优先级最高，但其实际占用很低（Opus 20ms 帧编码 <2ms），不会饿死 UI/BLE。

### 3.3 数据流总览

```
【状态流】 IDE ──agent_update──► app ──► LVGL list 屏幕
【指令流】 触摸/按键 ──touch/key_event──► IDE（可靠帧）
【音频流】 ES8311 mic → I2S DMA → Opus(20ms) → AudioChunk 帧 → BLE → IDE
           IDE 回复 ──tts 文本帧──► ESP 板载 TTS → PCM → I2S → 喇叭
           （备选：IDE TTS 合成 → Opus 下行 AudioChunk → ESP 解码播放）
【遥测流】 ESP ──Telemetry 帧(2s)──► IDE 状态栏 / ESP 详情面板
【心跳流】 双向 5s，15s 无帧判死
```

---

## 4. 目录 / 模块结构

```
Firmware/PlatformIO/
├─ platformio.ini                  # build_src_filter 需新增 +<boards/…> 之外无变化（src/ 子目录自动编译）
├─ src/
│  ├─ main.cpp                     # 入口：初始化 board/sys/ble/proto/audio/ui/app，启动任务
│  ├─ board/                       # 板级抽象层
│  │  ├─ board.h                   # BoardConfig 结构 + 外设句柄（每板一实例）
│  │  ├─ board_impl_c5.cpp         # Waveshare C5 实例化（引脚表 + 背光/电池/LCD/触摸）
│  │  └─ board_impl_c6.cpp         # Waveshare C6 实例化（同接口，备用板）
│  ├─ app/
│  │  ├─ app.h                     # App 类：状态机 + 消息路由（对 ui/audio/ble 的接口）
│  │  ├─ app.cpp                   # 状态机实现（§9）
│  │  ├─ mode_context.h            # ModeContext：当前模式 + 传入事件
│  │  ├─ mode_list.cpp             # 列表模式行为（翻页/长按菜单/新建按钮）
│  │  ├─ mode_settings.cpp         # 设置模式行为（行点击/滑块/开关）
│  │  └─ mode_master.cpp           # master 语音模式行为（PTT 状态/字幕/播报）
│  ├─ ble/
│  │  ├─ ble_nus.h / .cpp          # NUS 服务、广播、连接回调、MTU、重连
│  │  ├─ link.h                    # Link 抽象（send_frame / on_frame / status）
│  │  ├─ ble_link.cpp              # BLE 实现（默认）
│  │  ├─ usb_link.cpp              # USB CDC 实现（串口，供电+通信一体）
│  │  └─ wifi_link.cpp             # WiFi WS 实现（v2 预留，设置里存凭据）
│  ├─ proto/
│  │  ├─ frame.h / frame.cpp       # 帧编解码（§7.2，与 protocol.rs 一致）
│  │  ├─ crc16.h / crc16.cpp       # CRC-16/CCITT
│  │  ├─ seg.h / seg.cpp           # 消息分段/重组（§7.7）
│  │  ├─ reliable.h / reliable.cpp # seq/ack/重试窗口（§7.6）
│  │  ├─ heartbeat.h / .cpp        # 5s 心跳 / 15s 超时
│  │  └─ messages.h                # 消息类型常量 + JSON 载荷构建/解析辅助
│  ├─ audio/
│  │  ├─ audio.h / audio.cpp       # 采集/播放管理器（StartStream/StopStream/PlayTts）
│  │  ├─ codec_i2s.h / codec_es8311.cpp  # ES8311 驱动（I2C 寄存器 + I2S 通道）
│  │  ├─ codec_es7210.cpp          # 外部 4ch ADC（可选，复用 legacy S3 驱动思路）
│  │  ├─ opus_codec.h / .cpp       # libopus 封装（16kHz mono，16–24kbps，20ms 帧）
│  │  ├─ ptt.h / ptt.cpp           # BOOT 长按→PTT 事件
│  │  └─ barge_in.h / .cpp         # 播放中断（清缓冲）
│  ├─ ui/
│  │  ├─ lv_port.h / lv_port_disp.cpp  # ST7789 flush + LVGL draw buffer
│  │  ├─ lv_port_indev.cpp         # CST3530 → LVGL indev（+ 手势事件）
│  │  ├─ screens.h
│  │  ├─ screen_list.cpp           # 列表屏幕（4 卡片 + 新建 + 页点）
│  │  ├─ screen_settings.cpp       # 设置屏幕（行列表）
│  │  ├─ screen_master.cpp         # master 屏幕（状态 + 底部字幕）
│  │  └─ widgets.cpp               # 状态条 / toast / 对话框 / 长按菜单
│  ├─ sys/
│  │  ├─ tasks.h / tasks.cpp       # 任务创建与优先级
│  │  ├─ telemetry.cpp             # 电池 ADC 分压换算、heap、固件版本
│  │  └─ settings.h / settings.cpp # NVS 设置存取（§11）
│  └─ boards/                      # 保留现有（build_src_filter 选择性编译）
│     ├─ waveshare_lcd_28_c5/board.h
│     └─ waveshare_lcd_28_c6/board.h
└─ test/                           # 单元测试（proto/seg/crc 等纯逻辑可测）
```

> **分层规则**：`board/`、`proto/`、`audio/codec*` 不依赖上层；`ui/` 只依赖 `app/` 的只读模型；`app/` 是唯一同时连接 `ui`、`audio`、`ble/proto` 的编排点。这样后续把协议层提到 `lib/` 做独立单测也很容易。

---

## 5. 板级抽象层（board）

### 5.1 BoardConfig

`board.h` 以现有 `boards/waveshare_lcd_28_c5/board.h` 的宏为输入，导出统一结构，供上层使用：

```cpp
struct BoardConfig {
    // 显示
    uint16_t    width = 240, height = 320;
    int8_t      lcd_mosi, lcd_sclk, lcd_cs, lcd_dc, lcd_rst_exio; // EXIO=IO 扩展位
    // 触摸
    int8_t      tp_int, tp_rst_exio;   uint8_t tp_addr;
    // 音频（§5.3，待原理图确认）
    int8_t      i2s_mclk, i2s_bclk, i2s_ws, i2s_din, i2s_dout;
    uint8_t     codec_i2c_addr;  int8_t pa_ctrl_exio;   // 功放/背光
    // 按键 / 电池
    int8_t      boot_pin;   int8_t bat_adc_channel;
    // 能力
    bool        has_psram, has_wifi, has_ble, has_sd;
};
const BoardConfig& board_get();
```

每板一实现（`board_impl_c5.cpp`），编译期由 `-DBOARD_LCD_28_C5` 选择。现有 `board.h` 中的宏全部映射进结构体，上层不再直接引用 `GPIO_NUM_*`。

### 5.2 背光 / 功放

PA/背光共用 CH32V003 的 EXIO3，通过 IO 扩展器 I²C（0x24）控制。背光亮度 PWM 由 EXIO3 输出方波（或由代码实现软件 PWM 周期翻转）。`board_impl` 暴露 `set_backlight(0..100)` 与 `set_pa(bool)`。

### 5.3 音频引脚（待确认）

现有 `board.h` **未定义 I2S 引脚**。ES8311 的 MCLK/BCLK/WS/DIN/DOUT 需从原理图（`Hardware/Waveshare-C5-Touch-LCD-2.8/Docs/02-Schematics/`）提取，填入 `board_impl_c5.cpp`。设计预留两个 Codec 驱动：

- `codec_es8311.cpp`：C5 板载 Codec（ADC mic + DAC 喇叭），I²C 配置寄存器 + I2S 全双工。
- `codec_es7210.cpp`：外部 4 通道 ADC（legacy S3 板用的），C5 上可不用，保留 vtable 便于移植。

驱动接口（延续 legacy `capilot_audio` 的 vtable 思路）：

```cpp
struct CodecDriver {
    const char* name;
    bool (*is_present)();
    esp_err_t (*init)(uint32_t sample_rate, bool capture, bool playback);
    esp_err_t (*capture_start)();  esp_err_t (*capture_stop)();
    esp_err_t (*playback_start)(); esp_err_t (*playback_stop)();
    esp_err_t (*read)(int16_t* buf, size_t n);       // 阻塞读取 PCM
    esp_err_t (*write)(const int16_t* buf, size_t n); // 阻塞写入 PCM
    esp_err_t (*set_volume)(uint8_t pct);             // ES8311 DAC 增益
};
```

---

## 6. 传输层（ble / usb / wifi）

### 6.1 Link 抽象

```cpp
struct Link {
    enum Kind { Ble, Usb, Wifi };
    virtual Kind kind() = 0;
    virtual bool connected() = 0;
    virtual bool send(const uint8_t* frame, size_t len) = 0; // 可靠写（如 BLE WithResponse）
    virtual void set_on_frame(std::function<void(const uint8_t*, size_t)>) = 0;
    virtual void set_on_state(std::function<void(bool)> ) = 0;
    virtual void poll() {};      // 重连/心跳驱动（USB/WiFi 用）
};
```

v1 仅实现 `BleLink`；`UsbLink`/`WifiLink` 留接口（DevPlan §8.1 三实现，BLE 默认优先）。

### 6.2 NUS 服务（沿用现有 UUID）

| 特征 | UUID | 属性 | 方向 |
| --- | --- | --- | --- |
| Service | `6E400001-B5A3-F393-E0A9-E50E24DCCA9E` | — | — |
| TX | `6E400002-…` | `NOTIFY | READ` | ESP→IDE |
| RX | `6E400003-…` | `WRITE | WRITE_NR` | IDE→ESP |

- RX 保留 `WRITE_NR` 兼容；IDE 侧 `btleplug` 用 `WriteType::WithResponse`，ESP 侧 `onWrite` 对两者都处理。
- 广播名固定 `CaPilot-C5`（IDE `ble.rs` 按名字匹配）；建议在广播 Manufacture Data 中追加 2 字节固件版本号，便于 IDE 侧区分旧固件。

### 6.3 MTU 协商与帧长

- ESP 侧 `NimBLEDevice::setMTU(247)`（setup 时调用，允许中央协商更大 MTU）。
- 连接建立后读 `getMTU()`，`proto` 层据此设置 `max_payload = min(244, mtu - 3)`（ATT 头 3 字节）。
- **不假定**协商一定成功：帧头自带 len，`seg` 层按实际 `max_payload` 切段，天然兼容 20/23 字节小 MTU。
- 低吞吐下音频 16kbps≈2KB/s，20 字节 MTU 也够（50 帧/s × 20B = 1KB/s 接近但偏紧），故 MTU 协商仍是 P3 前置项（见 §12 里程碑 FW-B）。

### 6.4 连接参数与重连

- 连接后请求更新连接参数：interval 15–30ms、latency 0、timeout 2000ms（低延迟语音）。
- 断线回调 → `bleConnected=false` → **立即重启广播**；保留当前模式与设置，状态机回 `IDLE`。
- 重连成功后由 `app` 发 `connection_status` 给 IDE 并请求全量 `agent_update` 快照，完成对账。
- v1 不做配对（Just Works 可选）；配对留给安全加固阶段。

### 6.5 流量整形

- TX 用队列 + `ble_tx_task` 串行 notify，避免多任务并发 notify。
- 音频帧（50/s @16kbps）与遥测（0.5/s）混排；控制帧优先插队（`tx_priority`）。
- RX 写入回调只做：帧字节入 `rx_fifo`，由 `app`/`proto` 解码线程消费，**不在中断上下文解析 JSON**。

---

## 7. 帧协议（proto）

> 权威实现为 IDE 侧 `protocol.rs`；本节是 ESP 侧的对应实现规格。两者必须逐字节一致，并以共享测试向量验证（§12 FW-B）。

### 7.1 帧布局（与 `protocol.rs` 完全一致）

```
偏移  长度  字段
0     2     magic   0xCA 0x50
2     1     type    帧类型（§7.3）
3     2     len     载荷长度，小端 u16
5     1     seq     发送方序号（wrapping，控制帧用；音频帧作流内序号）
6     1     version 协议版本 = 0x01
7     len   payload 载荷（JSON 或二进制）
7+len 2     crc16   CCITT CRC-16，LE，覆盖字节 [2 .. 6+len]（即 type..payload）

帧总长 = 9 + len
```

**CRC**：poly `0x1021`，init `0xFFFF`，逐字节 `crc ^= byte<<8; 8 次移位异或`。与现 `main.cpp` 的 `crc16_ccitt()` 算法一致（但**覆盖范围与字段位置**按上表修正）。

**G1 迁移**：现 `main.cpp` 的 `buildTelemFrame()` 是 `magic|type|len|payload|crc`，无 seq/version —— 必须改写为 §7.1 布局，否则 IDE `try_decode()` 会把载荷首 2 字节当 seq/version、CRC 校验必然失败（见 §14）。

### 7.2 帧类型

| 值 | 枚举名 | 方向 | 可靠性 | 载荷 |
| --- | --- | --- | --- | --- |
| 0x01 | `Telemetry` | ESP→IDE | fire-and-forget | JSON 遥测 |
| 0x02 | `Command` | 双向 | 可靠（默认） | JSON 信封（§7.4） |
| 0x03 | `Ack` | 双向 | —（对可靠帧的回应） | JSON `{s, ok, err}` |
| 0x04 | `AudioChunk` | 双向 | fire-and-forget | 二进制音频头 + 载荷 |
| 0x05 | `Heartbeat` | 双向 | fire-and-forget（但被跟踪） | JSON（可空） |

帧类型枚举与 `protocol.rs` 的 `FrameType` 一致（Telemetry=0x01, Command=0x02, Ack=0x03, AudioChunk=0x04, Heartbeat=0x05）。

### 7.3 消息信封（Command 帧载荷）

Command 帧载荷为 JSON，`d` 为业务数据：

```json
{
  "t": "agent_update",     // 消息类型（§7.4 目录）
  "d": { },                // 业务载荷
  "q": 1,                  // 可选：可靠性覆盖。1=需 Ack，0=免 Ack（默认按 §7.5 策略）
  "ts": 1780000000000      // 可选：发送方 epoch 毫秒
}
```

### 7.4 消息目录

#### IDE → ESP

| `t` | 方向 | `d` schema | 说明 |
| --- | --- | --- | --- |
| `agent_update` | IDE→ESP | `{ "snap_seq": 12, "agents": [ { "id":"w1", "name":"worker-1", "status":"running", "role":"worker", "runtime":"claude", "action":"Editing user.py", "updated_ms":1780000000000 } ] }` | 全量快照或增量；ESP 重建列表模型。快照 ≤ 全部 agent |
| `audio_start` | IDE→ESP | `{ "mic":"board"\|"pc", "sr":16000, "bitrate":16000, "stream_id":1 }` | IDE 授权开始采集上行 |
| `audio_stop` | IDE→ESP | `{ "stream_id":1 }` | 停止上行流 |
| `subtitle` | IDE→ESP | `{ "origin":"user"\|"master", "text":"让文档 worker…", "partial":true }` | 实时字幕（STT 增量 `partial:true`，最终 `false`） |
| `tts` | IDE→ESP | `{ "text":"…", "voice":"zh_female", "stream_id":2, "barge":true }` | 回复文本帧下行 → ESP 板载 TTS 播报 |
| `connection_status` | IDE→ESP | `{ "state":"connected"\|"disconnected"\|"busy", "reason":"…" }` | IDE 侧连接/忙碌状态镜像 |
| `settings_sync` | IDE→ESP | `{ "settings": {…} }` | 全量设置推送（schema 见 §11） |
| `notify` | IDE→ESP | `{ "level":"info"\|"warn"\|"error", "text":"worker 已完成" }` | ESP 顶部 toast |
| `mode_set` | IDE→ESP | `{ "mode":"list"\|"settings"\|"master" }` | IDE 强制切模式（如语音启动时） |
| `action_result` | IDE→ESP | `{ "action":"rename", "agent_id":"w1", "ok":true, "err":null }` | ESP 发起动作（设 worker/重命名/删除）的执行结果 |

#### ESP → IDE

| `t` | 方向 | `d` schema | 说明 |
| --- | --- | --- | --- |
| `touch_event` | ESP→IDE | `{ "type":"tap"\|"long"\|"swipe_up"\|"swipe_down"\|"swipe_left"\|"swipe_right", "target":"agent:w1"\|"btn:new"\|"mode:list", "x":120,"y":56 }` | 触摸/手势事件 |
| `key_event` | ESP→IDE | `{ "type":"press"\|"short"\|"long", "source":"boot", "duration_ms":750 }` | BOOT 按键（长按>700ms=PTT；短按<300ms） |
| `mode_change` | ESP→IDE | `{ "mode":"list"\|"settings"\|"master", "reason":"gesture"\|"boot"\|"ide" }` | ESP 模式切换通知 |
| `mic_request` | ESP→IDE | `{ "req":"start"\|"stop", "source":"pc", "reason":"ptt" }` | mic 源=PC 时，ESP 请 IDE 录电脑麦克风 |
| `user_action` | ESP→IDE | `{ "action":"set_worker"\|"unset_worker"\|"rename"\|"delete"\|"open_detail", "agent_id":"w1", "name":"新名" }` | 列表长按菜单动作 |
| `audio_event` | ESP→IDE | `{ "stream_id":1, "event":"started"\|"stopped"\|"dropped":3 }` | 上行流状态 / 丢帧统计 |
| `log` | ESP→IDE | `{ "level":"debug", "msg":"…" }` | 调试日志 |

**遥测（帧 0x01）载荷**（在现有基础上扩展）：

```json
{
  "batt_mv": 3950, "batt_pct": 82, "uptime_s": 123,
  "heap_free": 45231, "status": "connected",
  "fw": "0.4.0", "mode": "list", "rssi": -55
}
```

**心跳（帧 0x05）载荷**：ESP→IDE `{"u":123}`；IDE→ESP 空载荷即可。

### 7.5 可靠性策略

| 消息 | 默认可靠性 | 理由 |
| --- | --- | --- |
| `agent_update` / `settings_sync` / `connection_status` / `mode_set` / `action_result` / `audio_start` / `audio_stop` / `tts` | **可靠** | 状态/命令丢失代价高 |
| `touch_event` / `key_event` / `mode_change` / `user_action` / `mic_request` | **可靠** | 用户操作不可丢 |
| `subtitle` / `notify` | **免 Ack** | 高频、可丢、语义可被下一条覆盖 |
| `telemetry` / `heartbeat` / `audio_chunk` | fire-and-forget | 语义为"最近值/连续流" |

每条消息可用信封 `"q"` 字段覆盖（如 `subtitle` 某些关键终态想确认，置 `q:1`）。

### 7.6 seq / ack / 重试

- 每侧维护 wrapping u8 发送序号；帧头 seq 为发送方自己的序号。
- 接收方对**可靠帧**回 `Ack`：`{ "s": <对方帧 seq>, "ok": true }`（失败 `{ "s":…, "ok":false, "err":"…" }`）。
- 发送方（`reliable.cpp`）：
  - 入窗口（最多 4 个未确认帧），`send_and_wait(seq)`；
  - 超时 500ms 未收到 Ack → 重发，最多 2 次（共 3 次）；仍失败 → 丢弃 + `log` 上报 + （对用户操作）toast 提示；
  - 收到 `ok:false`（如 `busy`）→ 退避 1s 后重发 1 次。
- 重复帧防护：接收方记录最近 N 个已处理 seq（环形），重复即忽略但**仍回 Ack**（防止发送方误重发）。
- 音频帧、遥测、心跳**不回 Ack**。

### 7.7 分段 / 重组（seg）

BLE MTU 小（无协商时 20B），而 `agent_update` / 长 `subtitle` / `settings_sync` 可能超 `max_payload`。`seg` 层在 frame payload 首字节打分段标记：

```
payload[0]==0xAA  → 分段帧：[0xAA | msgid(1) | frag_idx(1) | frag_total(1) | chunk…]
payload[0]=='0x7B'({) → 单帧消息（原始 JSON）
```

- 发送：大消息切成 ≤(max_payload−4) 字节的块，`frag_total` 固定，`frag_idx` 0..n−1；每块以独立帧 + 独立 seq 发送、独立 Ack（可靠消息才分段）。
- 接收：按 `(msgid)` 收集，齐 `frag_total` 后拼回 JSON 再解析；超时 2s 未齐 → 丢弃整组并回 `ok:false` 给最后一块。
- BLE NUS 写（WithResponse）与 notify 在链路上有序，重组只需按序拼接。

### 7.8 心跳与超时

- 双方每 **5s** 发一次 `Heartbeat`（ESP 可并入遥测任务）。
- 任一侧若 **15s** 内未收到任何有效帧（含任意类型）→ 判定链路失效：
  - ESP 侧：`bleConnected=false`、重启广播、状态机回 `IDLE`，同时通知 IDE `connection_status{state:disconnected}`（若能发出）；
  - IDE 侧：状态栏闪烁提示，进入重连流程。

### 7.9 协议状态与对账

- 连接建立后，双方交换一次握手：
  - ESP：发 `Telemetry`（fw/mode）+ 收 IDE 的 `connection_status{connected}`；
  - ESP 收到 `connection_status{connected}` 后发 `key_event` 或直接请求 `agent_update` 快照 → 对账列表。
- 协议版本不匹配（`version != 0x01`）：ESP 记录 + toast"固件/IDE 版本不匹配"，仍收遥测请求但不渲染业务。

---

## 8. 音频管线（audio）

### 8.1 上行：ESP mic → Opus → BLE → IDE

```
BOOT 长按(>700ms) ──► PTT 事件
  ├─ mic_source=board：audio_task 开始：ES8311 ADC → I2S DMA
  │    → 20ms 块（320 样本 = 640B PCM）→ Opus 编码（16kHz mono，16–24kbps，20ms 帧）
  │    → AudioChunk 帧（§8.3）→ ble_tx 队列 → notify
  └─ mic_source=pc：发 mic_request{start,pc} → IDE 录电脑麦 → STT（ESP 不采音）
松键 ──► PTT 释放 → 发最后一帧（flags.final=1）→ audio_stop/或 mic_request{stop,pc}
```

参数（DevPlan §9）：`16kHz / mono / 16-bit PCM → Opus 16–24kbps / 20ms 帧`。

### 8.2 下行：回复文本 → ESP TTS 播报（barge-in）

```
IDE ──tts 文本帧──► ESP app ──► audio::PlayTts(text)
  → 板载 TTS（esp-tts，zh 音色数据在 flash）合成 16kHz mono PCM
  → ES8311 DAC → I2S → 喇叭（PA 使能 EXIO3）
播报中再长按 BOOT ──► barge-in：flush I2S 缓冲 + 丢弃队列 → 直接进入新一轮 PTT 录音
播报中短按 BOOT ──► 仅停止/打断播报（PRD H2），不开始录音
```

**备选下行（PRD H5 兼容）**：若板载 TTS 质量不足或 C5 不支持 esp-sr，则 IDE 侧用本地 TTS 合成 → Opus 编码 → 下行 `AudioChunk`（方向位=下行）→ ESP Opus 解码 → PCM → 喇叭。两种路径共用 §8.3 帧格式，由 `tts` 消息的 `"engine":"esp"|"ide"` 决定。

### 8.3 AudioChunk 二进制载荷

```
偏移  长度  字段
0     1     stream_id   bit7=方向（0 上行 ESP→IDE / 1 下行 IDE→ESP），低 7 位流号
1     2     seq         流内序号（LE u16，wrapping，用于丢帧检测）
3     1     flags       bit0=first  bit1=final  bit2=redundant(补发)
4     1     codec       0=Opus  1=PCM16 原始
5     n     payload     Opus 帧或 PCM
```

- 20ms Opus@16kbps ≈ 40B → 每帧 45B；50 帧/s ≈ 2.2KB/s（含头）。BLE 10–30KB/s 实测余量 6 倍。
- **丢帧策略**：实时语音不缓冲、不重传。若 BLE 未连接或 TX 队列满，直接丢帧并累计 `dropped` 计数上报 `audio_event`。音频质量优先"低延迟 + 连贯"，偶尔丢 1–2 帧 Opus 可接受（Opus 有 PLC）。
- IDE 侧 `audio.rs` 解 AudioChunk → Opus 解码 → sherpa-onnx 流式 STT（文字不过 webview）。

### 8.4 缓冲设计

| 缓冲 | 大小（无 PSRAM 预算） | 说明 |
| --- | --- | --- |
| I2S DMA RX/TX | 2×(1–2KB) DMA buffer | 硬件直接搬运，不占 CPU |
| PCM 采集块队列 | 4×640B ≈ 2.5KB | audio_task 与编码之间 |
| Opus 编码器工作区 | ~10–20KB 静态 | `opus_encoder_create` 分配 |
| TX BLE 帧队列 | 8 帧（音频优先丢弃） | 满则丢音频帧 |
| TTS 播放缓冲 | 8–16KB（高水位/低水位） | 下行过满则丢（barge-in 清空） |

### 8.5 延迟预算（目标 E2E ≤3s）

| 段 | 预算 |
| --- | --- |
| 采集 → Opus 编码 | <50ms |
| BLE 上行（50 帧/s） | <300ms（含连接间隔） |
| IDE 解码 → STT 首字 | <800ms（sherpa-onnx 流式） |
| master 响应（LLM 首 token） | ~1–1.5s（主预算） |
| 下行 tts 文本帧 → 板载 TTS 首音 | <200ms |
| **合计（说话结束→master 开始响应）** | **≤3s** |

> 用户松开 PTT 时即发送 `final` 帧；IDE 以 VAD 停顿（或 final 帧）触发 STT 停止。DevPlan §9 的"VAD 停顿→停止"由 IDE 侧 VAD 承担，ESP 侧不做 VAD。

### 8.6 mic 源切换

- 设置项 `mic_source`（board/pc）保存在 NVS，并随 `settings_sync` 双向同步。
- `pc` 源：PTT 时不启动本地采集，仅发 `mic_request`；字幕仍显示（文字来自 IDE 的 `subtitle` 帧）。
- `board` 源：本地采集 + Opus 上行，同时 IDE 可另存 raw/Opus 供诊断。

---

## 9. 应用状态机（app）

### 9.1 顶层状态机

```
              ┌───────────┐
   上电 ──►   │   BOOT    │  初始化 board/sys/ble/proto/audio/ui，启动任务
              └─────┬─────┘
                    ▼
              ┌───────────┐   开始广播，显示"等待 IDE…"
              │   IDLE    │ ◄────────────┐
              └─────┬─────┘              │
                    │ 中央连接            │ 断线/15s 无帧
                    ▼                    │
              ┌───────────┐   MTU 协商/握手 ──┐
              │ CONNECTING│                  │
              └─────┬─────┘                  │
                    │ 握手完成(version 校验 + agent_update 对账)
                    ▼                        │
              ┌───────────┐                  │
        ┌────►│ CONNECTED │─────────────────►│（回 IDLE）
        │     └─────┬─────┘                  │
        │           │ 模式活动（§9.2）        │
        │           │                        │
        └───────────┘  全局事件可随时中断/切模式
```

全局事件（任何状态有效）：
- `key_event{long}`（BOOT 长按）→ 若不在 master 语音活动 → 切入/唤起 **master 语音 PTT**（§9.3）；
- `key_event{short}` → 按当前模式语义（列表=进选中详情；master=打断播报；设置=无）；
- `mode_change` / `mode_set` → 切模式屏幕；
- `swipe_left/right` → 按手势映射切模式（§10.3）。

### 9.2 模式活动（CONNECTED 内）

```
CONNECTED
 ├─ MODE_LIST      ── swipe_left ──────────► MODE_SETTINGS
 │   子状态：page_idx、selected_card         ── swipe_left ──┐
 │   长按卡片 → context menu（set_worker/rename/delete）      │
 │   垂直 swipe → 翻页（4/页 + 页点）                         │
 └─ MODE_SETTINGS ── swipe_right ──────────► MODE_LIST        │
 │   子状态：panel_scroll、focus_row                         │
 │   行交互：滑块/开关/列表 → 本地生效 + settings_sync 上行   │
 └─ MODE_MASTER    ── swipe_left ──────────► MODE_LIST ◄──────┘
    子状态：idle → recording（PTT 按住）→ processing（等待回复）
            → playing（TTS 播报）→ idle
```

### 9.3 master 语音模式子状态机

```
                 key:long 按住                key:long 松开
 MODE_MASTER.IDLE ─────────────► RECORDING ─────────────► PROCESSING
      ▲                            │  (Opus 上行 / 或 mic_request)    │
      │                            │  key:short(打断播报)             │ tts 文本帧下行
      │                            ▼                                 ▼
      │◄────────────  PLAYING ◄───────────────────────────────── 等待回复
      │                (TTS 播报)       字幕同步显示
      └──── tts 结束/打断 ─────┘
```

- `RECORDING`：mic 上行或 `mic_request`；松开键发 `final` 音频帧。
- `PROCESSING`：屏显"思考中…"，字幕滚动 STT 增量（`subtitle{origin:user,partial:true}`）。
- `PLAYING`：收到 `tts` 文本帧 → 板载 TTS 播报；期间 BOOT 长按 = barge-in 回 `RECORDING`；短按 = 打断回 `IDLE`。
- 触摸/滑动在 master 模式下仍可退出（swipe_left → list）。

### 9.4 事件路由（app → 各模块）

`app::dispatch(event)` 按 `(state, mode)` 分发到 `mode_*.cpp`；每个 mode 返回 `ActionResult{send_messages[], ui_action, mode_next}`。所有要发往 IDE 的消息统一在 `app` 经 `proto::send_reliable/send_fire` 出口，保证 seq/ack 语义一致。

---

## 10. UI 设计（ui / LVGL）

### 10.1 LVGL 配置要点

- LVGL 9.2，`LV_COLOR_DEPTH=16`，无 PSRAM → **局部缓冲渲染**：
  - 两个 draw buffer `240×40×2 ≈ 19.2KB` 各一，共 ~38.4KB；`LV_DISP_RENDER_MODE_PARTIAL`。
  - 不打 `LV_USE_SNAPSHOT`（现有 build_flags 已关闭）。
- **中文字体是必须项**：LVGL 内置字体无 CJK。用 LVGL 字体转换工具生成常用 3000 字 16px/20px 字体子集（存入 flash，约 0.5–1MB；16MB flash 充裕），供字幕/卡片/设置文案使用。图标用 Font Awesome 子集或简单符号。
- 触摸：CST3530 → LVGL `indev`（`LV_INDEV_TYPE_POINTER`）；手势经 `lv_indev` 的 gesture 事件（或由 `touch.cpp` 基于坐标序列自行判定）产生 `swipe_*` 事件。

### 10.2 三屏幕布局（240×320）

```
列表模式                             设置模式                          master 模式
┌──────────────┐ 40px 状态条      ┌──────────────┐ 状态条           ┌──────────────┐
│ 列表 ●BT 82% │                 │ 设置 ●BT 82% │                  │ master ●BT 82%│
├──────────────┤                 ├──────────────┤                  ├──────────────┤
│ [icon] w1 ●  │ 卡片1           │  WiFi      › │ 行1              │              │
│ 正在编辑 x   │                 │  音量   ──●── │ 行2（滑块）      │  波形/留白    │
├──────────────┤                 │  亮度   ──●── │ 行3              │  （或 master  │
│ [icon] w2 🔄 │ 卡片2           │  麦克风源 板载│ 行4（分段）      │   状态大字）   │
├──────────────┤                 │  智能返回  [ ]│ 行5（开关）      │              │
│ [icon] e3 ✗  │ 卡片3(闪)       │  STT 引擎 sherpa │ 行6           │              │
├──────────────┤                 │  TTS 音色 默认 │ 行7              │              │
│ [icon] i4 💤 │ 卡片4           │  字幕开关  [x]│ 行8              │              │
├──────────────┤                 │  …           │ 滚动            ├──────────────┤
│ [+ 新建 agent]│ 56px 固定按钮  │              │                  │ ▌让文档 worker │
├──────────────┤                 └──────────────┘                  │  把接口写进…  │
│     ● ● ○    │ 页点（3 页）                                      │ 用户(左)      │
└──────────────┘                                                    │ master(右)   │
                                                                    └──────────────┘ 字幕行
```

- 状态条：模式名 + 连接状态（BT 图标）/ 电量 + 未读/toast 区。
- 列表卡片：状态图标（✅/🔄/💤/🤖/✗）+ 名称 + worker 标记；第二行当前动作文字；error 状态卡片闪烁（`lv_anim` 透明度/背景）。
- 长按卡片（500–700ms 视觉反馈：背景加深/波纹）→ 弹出菜单：设为/取消 worker、重命名、删除并关闭（二次确认对话框）。
- 空态："去 IDE 新建 agent"。

### 10.3 手势映射（PRD H4）

| 当前位置 | 手势 | 动作 |
| --- | --- | --- |
| 列表 | 左滑（dx>60px 且 dx>dy） | → 设置 |
| 列表 | 右滑 | → master |
| 列表 | 垂直滑动 | 翻页（不与横滑冲突） |
| 设置 | 右滑 | → 列表 |
| master | 左滑 | → 列表 |

映射存 NVS 设置，可自定义/关闭任一方向（`gesture_map`，§11）。

### 10.4 屏幕生命周期

`screen_manager` 采用"单实例 + 显隐"而非销毁重建（240×320 资源有限）；数据更新经 `app` 的只读模型推送（`list_model` / `settings_model` / `subtitle_model`）。收到 `agent_update` → 更新模型 → `lv_label/lv_anim` 局部刷新，避免全屏重绘。

---

## 11. 设置与持久化

### 11.1 设置 schema（与 IDE 共享一份）

```json
{
  "settings": {
    "wifi":       { "ssid":"", "password":"", "enabled":false },
    "volume":     70,
    "brightness": 80,
    "mic_source": "board",
    "smart_return": true,
    "stt_engine": "sherpa",
    "tts_engine": "esp",
    "tts_voice":  "zh_female",
    "subtitle":   true,
    "gesture":    { "left_to":"settings", "right_to":"master" }
  }
}
```

### 11.2 双向同步

- **ESP → IDE**：设置模式中用户改动任一项 → 本地 NVS 立即保存 + 发 `settings_sync`（可靠帧）。
- **IDE → ESP**：IDE 设置页改动 → 推 `settings_sync`；ESP 应用并回 Ack；若与本地不同（如 IDE 重启后旧值）以**最后写入者**为准并回推合并值。
- 应用项：`volume`→ES8311 DAC 增益；`brightness`→EXIO3 背光；`mic_source`→§8.6；`subtitle`→master 屏幕显示；`wifi`→仅存储 + v2 启用 WiFi 传输；`smart_return`/`stt_engine`/`tts_*`→ESP 只存储转发（IDE 侧生效），但 `tts_engine` 决定 §8.2 下行路径。

### 11.3 持久化（NVS）

| Key | 内容 |
| --- | --- |
| `cfg` | 设置 JSON（ArduinoJson 序列化） |
| `last_mode` | 上次模式（重启恢复） |
| `ble_name` | 可覆盖广播名（默认 CaPilot-C5） |
| `pair_token` | v2 配对/安全 token（预留） |

---

## 12. 里程碑与任务拆分

> 与 DevPlan P3（≈2 周）并行；每阶段独立可验证。前置：P0.5 BLE 吞吐 spike 已完成、C5 硬件在案。

| 阶段 | 时间 | 内容 | 验证标准 |
| --- | --- | --- | --- |
| **FW-A 板级 bring-up** | 1–2 天 | LCD+LVGL 渲染、CST3530 触摸、**ES8311 录音/放音 demo**（PRD 风险前置）、BOOT 长按/短按判定、背光/电池 | 实机显示 + 触摸 + 录音回放通 |
| **FW-B 协议层** | 1–2 天 | 帧布局升级（G1，seq/version）、CRC 测试向量、seg、reliable、心跳、MTU 协商、遥测扩展（fw/mode/rssi） | 与 IDE `protocol.rs` 测试向量互验；ESP↔IDE BLE 遥测+心跳通 |
| **FW-C 列表模式** | 2–3 天 | `agent_update` 快照→列表模型、4 卡片+页点+垂直翻页、长按菜单（设 worker/重命名/删除）、新建按钮、`touch_event`/`user_action` 上行 | 在 IDE 建/改 agent → ESP 屏实时一致；长按操作回写 IDE 生效 |
| **FW-D 设置模式** | 1–2 天 | 设置行 UI（滑块/开关/分段）、`settings_sync` 双向、音量/亮度/mic 源本地生效、NVS 持久化 | 改 ESP → IDE 设置页同步；改 IDE → ESP 生效；重启不丢 |
| **FW-E master 语音** | 3–4 天 | Opus 上行（audio_start/stop、AudioChunk）、`mic_request`（PC 源）、`subtitle` 实时字幕、TTS 播报（esp-tts 或 IDE 下行）、barge-in、PTT 状态机 | 长按说话 → IDE 出字；回复喇叭播报；播报中打断通；E2E ≤3s |
| **FW-F 加固** | 1–2 天 | 断线重连/对账、低吞吐降级（小 MTU 也能跑）、中文字体子集打磨、丢帧统计、日志/`log` 帧、OTA 预留 | 拔线重连恢复；内存峰值 <70% SRAM；无卡死 24h |

**关键路径**：FW-A 的 ES8311 demo 是 FW-E 的前置；FW-B 是 FW-C/D/E 的公共底座。建议 FW-A 与 FW-B 与 IDE P3 的 `EspTransport`/协议帧同步联调。

---

## 13. 风险与待验证

| 风险 | 说明 | 应对 |
| --- | --- | --- |
| **C5 无 PSRAM** | 与 PRD 宣称冲突；LVGL/音频/Opus 全挤 ~400KB SRAM | §3.2 任务模型 + 局部渲染 + 内存预算（~200KB 内）；实机验证后如需可换 C6 或开 PSRAM |
| **ES8311 引脚/驱动未验证** | board.h 无 I2S 引脚 | FW-A 前置：从原理图提取引脚，跑通录音/放音 demo |
| **esp-sr/esp-tts 是否支持 C5** | esp-sr 组件对新芯片可能滞后 | 若不支持 → §8.2 备选路径：IDE 侧 TTS → Opus 下行，同帧格式切换 |
| **BLE MTU 协商** | btleplug 平台差异（Win/mac 默认 MTU 不同） | ESP 主动 `setMTU(247)` + seg 层自适应 max_payload |
| **BLE notify 丢失/乱序** | BLE 链路偶发 | 控制帧 ack/重试；音频 fire-and-forget + 丢帧检测 |
| **单核抢占** | audio/UI/BLE 争 CPU | 优先级设计 + Opus 编码极快；超时保护 |
| **CJK 字体体积** | 中文字体子集体积 | 只收常用 3000 字；必要时动态按需加载 |
| **端到端 ≤3s** | 硬指标 | FW-E 单独延迟测量；sherpa 流式 + 板载 TTS 低延迟 |

---

## 14. 兼容性与迁移

### 14.1 现 `main.cpp` → 新结构迁移步骤

1. 拆分 `main.cpp` 为 `board/`、`ble/ble_nus.cpp`（保留 NUS UUID/广播逻辑，抽出 MTU/重连）。
2. `proto/frame.cpp` 重写帧构建：**插入 seq+version**，修正 CRC 覆盖范围到 `type..payload`（§7.1）。
3. `proto/reliable.cpp` 接上 RX 的 Ack 回发（替换现有"收到写就回 ok"的裸逻辑，改为按 seq 回）。
4. `sys/telemetry.cpp` 扩展遥测 JSON（fw/mode/rssi）。
5. 随后按 FW-C/D/E 逐个接入 UI/音频。

### 14.2 旧固件兼容

- IDE 侧 `protocol.rs` 已按 9 字节头实现；旧固件（无 seq/version）的帧会在 IDE 端 `BadMagic`/`CrcMismatch` 后按字节重同步，最终表现为"连上但无遥测"——升级固件即解决。广播名保持 `CaPilot-C5` 不变，便于 IDE 扫描逻辑兼容。

### 14.3 协议版本演进

`version=0x01`；未来加字段时：保持帧头固定，`version` 递增，接收方对不支持的 version 发 `connection_status{state:unsupported}` 并在 UI 提示升级，避免静默错配。

---

## 附录 A：帧格式速查

```
┌──────┬──────┬──────┬──────────────┬─────┬──────┬──────────────┬──────────┐
│ 0xCA │ 0x50 │ type │ len (LE u16) │ seq │ vers │ payload[len] │ crc16 LE │
└──────┴──────┴──────┴──────────────┴─────┴──────┴──────────────┴──────────┘
   2B     1B      1B        2B         1B     1B      len          2B
CRC = crc16_ccitt(type..payload)   // 字节 [2 .. 6+len]
总长 = 9 + len
type: 0x01 Telemetry | 0x02 Command | 0x03 Ack | 0x04 AudioChunk | 0x05 Heartbeat
```

## 附录 B：消息目录速查

| 方向 | 消息 `t` | 可靠性 | 帧类型 |
| --- | --- | --- | --- |
| IDE→ESP | `agent_update` `settings_sync` `connection_status` `audio_start/stop` `tts` `mode_set` `action_result` | 可靠 | Command(0x02) |
| IDE→ESP | `subtitle` `notify` | 免 Ack | Command(0x02) |
| ESP→IDE | `touch_event` `key_event` `mode_change` `user_action` `mic_request` `audio_event` `log` | 可靠（`log` 可免） | Command(0x02) |
| ESP→IDE | 遥测 | fire-and-forget | Telemetry(0x01) |
| 双向 | 心跳 | fire-and-forget | Heartbeat(0x05) |
| 双向 | 音频 | fire-and-forget | AudioChunk(0x04) |
| 双向 | 确认 | — | Ack(0x03) |

## 附录 C：与 DevPlan §8.2 / §9 的对照

| DevPlan 要求 | 本文实现 |
| --- | --- |
| 帧 `magic+type+len+payload+crc16 + seq+version` | §7.1 与 `protocol.rs` 逐字节一致 |
| 控制帧 ack/重试；音频帧 fire-and-forget | §7.5/§7.6 |
| IDE→ESP：agent_update/audio_start/subtitle/connection_status | §7.4 消息目录（全部覆盖） |
| ESP→IDE：touch_event/key_event/mode_change/audio_chunk/mic_request | §7.4（audio_chunk = AudioChunk 帧） |
| 心跳 5s 双向 / 15s 超时 | §7.8 |
| ESP mic→Opus(16kHz/16–24kbps)→BLE；Rust 解码→sherpa STT→实时文字 | §8.1/§8.5 |
| 回复文本帧下行→ESP TTS 播报（barge-in） | §8.2 |
| 无波形、音频不过 webview | 上行 Opus 至 Rust 侧；webview 只收文字 |
| 三模式镜像（列表/设置/master） | §9.2 / §10.2 |
