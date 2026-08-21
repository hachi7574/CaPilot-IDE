# 画布新建终端 — 对齐 cleancode 的运动学

> **日期:** 2026-08-21  
> **状态:** 步骤 A–E 已在代码落地（2026-08-21）。验收以画布里「新建终端」为准。  
> **对照:** `/home/hachi/Project/cleancode` 的 workbench object / viewport 弹簧  
> **不要做:** 引入 `@xyflow/react`、tldraw；不要再在每张卡上写 `transform: scale(zoom)`

当前用户看到的两件事：

1. **新建终端后画布上没有新卡**（停掉自动投影之后，入图路径断了）  
2. **即便出现，也和 cleancode 不一样**（所有卡闪/缩、镜头不跟）

必须先修 1，再按下面分层修 2。上一步没绿不要跳。

---

## 0. 为什么现在会「没有新卡」

停掉 `mergeAgentsIntoGraph` 的自动网格投影之后，卡只能来自 **已写入的 graph.json**（`terminals[]` / `agents[]`）。

现在的创建链：

```
工具栏/右键 → TerminalTemplatePicker
  → spawnTerminal() → spawnAgent()
       → addAgent + **addTab(agent)**   ← 立刻把 activeTab 切成终端视角
  → onSpawned(id) → afterSpawn → placeAgentOnCanvas
  → picker onClose → stayOnCanvas()
```

断点：

| 点 | 发生什么 | 后果 |
| --- | --- | --- |
| A | `spawnAgent` 里 `addTab({ type: "agent" })` | `CanvasPanel` **卸载**（画布 tab 不是 resident） |
| B | `onSpawned` 在卸载后的闭包里调 `placeAgentOnCanvas` | `setGraph` 打在已死实例上，**图没写上** |
| C | `stayOnCanvas` 再挂上新的 `CanvasPanel` | `canvas_graph_get` 读到的还是旧图 → **没有新卡** |
| D | 即便 B 偶尔写上了 | merge 不再自动投影，只认 graph 里的节点 |

cleancode 没有这个问题：创建物体时 **工作台一直挂着**，节点进 React Flow store，viewport 弹簧打在**同一层 DOM** 上。

**本步验收（修看不见）：** 画布里点「新建终端」→ 选模板 → **不离开画布** → 出现一张新卡。侧栏也会多一条 session。`pnpm tsc --noEmit` 绿。

---

## 1. cleancode 实际在做什么（抄这个，不要再发明）

### 1.1 两套弹簧，互不打在同一批 DOM 上

| 弹簧 | 文件 | 打在谁身上 | 改什么 |
| --- | --- | --- | --- |
| **物体出现** | `workbenchObjectSpring.ts` + `useWorkbenchObjectMotionPresentation.ts` | **仅新节点的锚点** `.terminal-node-anchor` | `--workbench-object-motion-scale` 0→1，`--…-opacity` 0.16→1 |
| **镜头** | `workbenchViewportMotion.ts` → `applyWorkbenchViewport` | **一层** `.react-flow__viewport` | `translate + scale`，节点 `style.left/top` **不变** |

物体弹簧参数（create）：

```
dampingRatio = 1          // 临界阻尼，无回弹过冲
response     = 0.34s      // presenceCreateSpringDynamics
scale.from   = 0
scale.to     = 1
opacity.from = 0.16（实现里 create 用 scale 轴；锚点 CSS 另有淡入）
transform-origin: center
```

积分器就是 `motionSpring.ts` 的 `advanceSpringStep`（临界阻尼闭式）。**不要** CSS `@keyframes` 回弹曲线。

镜头对焦（`useCanvasSelectionViewport.ts`）：

```
center = 节点世界中心
zoom   = 现有 zoom（或 adaptive-focus 算一档，新建时保持当前 zoom 即可）
intent = adaptive-focus / spatial
每一帧只 apply viewport，不 setState 到每个 node
```

### 1.2 节点怎么避免「全体闪」

- `TerminalNode` 是 `memo` 的。  
- viewport 变 → React Flow 只改 viewport 层，**不 re-render 每个 TerminalNode**。  
- create 时 `objectMotion` 只挂在**新 node.data** 上。  
- 弹簧 **只改 CSS 变量**，不卸 children，xterm 不重挂。  
- 已有节点的 computed style 在 create 那一帧不变。

### 1.3 创建时序（cleancode）

1. 算世界坐标（不与已有节点重叠）。  
2. 节点进图（唯一真相）。  
3. 同一帧：新节点带 `objectMotion: { kind: 'create', scale: {from:0,to:1} }`。  
4. 镜头弹簧开始（另一条 rAF）。  
5. 节点弹簧在锚点上播完 → `onObjectMotionComplete` 清掉 motion。  
6. **工作台 DOM 全程不卸载。**

---

## 2. CaPilot 目标架构（对齐上面，不引入 xyflow）

```
.canvas-surface                  // 指针、滚轮、drop
  .canvas-edges                  // 屏幕坐标的边（可后做）
  .canvas-world                  // 唯一 viewport：translate(x,y) scale(z)
                                 // 只通过 DOM style 写，禁止 JSX 每帧绑定
    .canvas-node                 // position:absolute; left/top = 世界坐标
                                 // width/height = 展开尺寸（700×700），无自身 scale(zoom)
      [.canvas-node-appear]      // 仅新卡：scale(var(--scale))，origin center
        CanvasNodeCard           // memo；内部自己订 agentId；内部挂 XTermPanel
```

硬规则：

| 禁止 | 原因 |
| --- | --- |
| 每张卡 `style.transform = scale(zoom)` | setViewport 会改每张卡的 style → 全体闪缩 |
| JSX `style={{ transform: viewport }}` 绑在 `.canvas-world` | 任意 setState 会盖掉镜头弹簧正在写的 transform |
| `CanvasAppear` 在有/无动画之间切换 fragment↔div | 卸载 XTermPanel → 所有可见 PTY 闪 |
| `useStore(s => s.agents)` 订整个 Map | session 状态一变，整张画布重绘 |
| `mergeAgentsIntoGraph` 自动网格投影未入图的 session | 先出现在网格再跳到空位；和「只有拖入/新建才上画布」冲突 |
| 新建时 `spawnAgent` → `addTab(agent)` | 卸载 CanvasPanel，入图写丢 |

---

## 3. 分步实现

### 步骤 A — 画布内 spawn 不准切走 tab（修「看不见」）

**改:** `ui/state/agentActions.ts` 的 `spawnAgent`，或画布专用 `spawnAgentForCanvas`。

建议：

```ts
spawnAgent(project, runtime, { addTab?: boolean })
```

画布创建走 `addTab: false`。Session 进 `agents` Map 和 sqlite，**不** `setActiveTab`。

Picker：

```ts
spawnTerminal(project, tpl, { addTab: false }).then(id => onSpawned(id))
```

`afterSpawn` 必须在 **仍挂着的** CanvasPanel 上调用 `placeAgentOnCanvas`。

`placeAgentOnCanvas` 必须 `setGraph` 写入 `terminals[]` 或 `agents[]`（已有逻辑），然后 `persist`。

**本步验收:** 画布新建 → 不切走 → 刷新/再进画布卡还在。DevTools 里 `.canvas-node` 数量 +1。不要看动画。

### 步骤 B — 先定空位再入图（禁止重叠、禁止先投影再搬）

在 `placeAgentOnCanvas`、`motion === "create"`：

1. 用当前 **已展开尺寸**（默认 `EXPANDED_MIN` 700×700）和间距（32px）对已有节点做 AABB。  
2. `findFreeWorldPos(preferred, occupied, size)`（已有函数）得到 `dest`。  
3. **只把 `dest` 写入 graph**，不要先让 merge 用网格位画一张。  
4. 侧栏拖入（`drop`）用落点，不找空位。

**本步验收:** 连续新建 3 张，互不重叠。没有「先出现在左上角再飞走」。

### 步骤 C — 镜头只弹簧 `.canvas-world`（抄 viewport 弹簧，不 setState 每帧）

从 cleancode 抄积分，不要抄 React Flow API：

- `advanceSpringStep` 临界阻尼闭式（`motionSpring.ts` 77–83 行）  
- create 对焦：`response = 0.36`（spatial）或 0.34  
- 目标：

```
zoom' = 当前 zoom（不要乱改）
x' = viewW/2 - (dest.x + size.w/2) * zoom
y' = viewH/2 - (dest.y + size.h/2) * zoom
```

实现约束：

- rAF **只** `worldRef.current.style.transform = translate() scale()`  
- 弹簧进行中 **禁止** `setViewport`  
- 结束时 **一次** `setViewport(target)` + `persist`  
- `useLayoutEffect([viewport])` 仅在 `camRafRef === null` 时把 state 写回 DOM（防止 React 覆盖飞行中的 transform）  
- `.canvas-world` 的 JSX **不要**绑 `style.transform`

**本步验收:** 新建时已有卡的 `getBoundingClientRect()` 相对彼此不变（整体平移），没有单独缩小。镜头平滑移到新卡中心。中途点空白拖动画布应 `cancel` 飞行。

### 步骤 D — 物体弹簧只打新卡（抄 object spring 的呈现，不抄 xyflow）

抄自 `createAtomicObjectCreationMotion` + `workbench-object-motion.css` 锚点规则：

- 新卡第一帧就带壳 `.canvas-node-appear-create`（scale 变量默认 0）。  
- 弹簧写 `--canvas-appear-scale` / `--canvas-appear-opacity`（和 cleancode 一样用 CSS 变量，不改 `element.style.transform` 字符串，以免和以后的拖拽抢）。  
- `transform-origin: center`。  
- **不要**在动画结束时拆掉壳再换成 fragment（会卸 xterm）。壳可以一直留着，结束后变量设为 1 或去掉 create class、保留空壳。  
- 已有卡：**不要**包带 `scale(var(--canvas-appear-scale))` 的层。  
- `CanvasNodeCard` 保持 `memo`，xterm 画在卡内，`showPty` 为 boolean。  
- 画布 **不要** `useStore(s => s.agents)` 订整个 Map；用 `agentId` 签名或让卡自己订。

参数必须和 cleancode 一致：

```
create: dampingRatio=1, response=0.34, scale 0→1, opacity 0.16→1
drop:   response=0.24（侧栏拖入；可后做重力，先弹簧即可）
settled: |v-1|<0.002 且 |vel|<0.02
```

**本步验收:** 新建时 **只有新卡** 从中心长大。DevTools 里已有卡的 `transform` 在创建过程中不变。新卡内 PTY 不闪成白屏再出字（允许空壳先出现、xterm 稍后 fit，但不要卸挂）。

### 步骤 E — 创建编排（一条函数，禁止散弹 setState）

画布专用：

```ts
async function createTerminalOnCanvas(scope, template, preferredWorld: CanvasVec)
```

顺序（单线程，不要并行 setState 打架）：

1. `dest = findFreeWorldPos(...)`  
2. `id = await spawnAgent(..., { addTab: false })`  
3. `pendingAppearRef[id] = 'create'`  
4. `setGraph` 写入 dest（**一次**）  
5. 启动镜头弹簧（DOM）  
6. 新卡 mount → `CanvasAppear` 读 ref 开物体弹簧  
7. 两套弹簧独立结束  

不要：`stayOnCanvas` 来回切 tab；不要 `addTab` 再 `setTimeout` 切回来。

**本步验收:** 从点模板到新卡可见 < 300ms 量级；镜头与弹出同时开始；已有卡无缩放闪。

### 步骤 F — 回归（必须全过）

- [ ] 画布新建：新卡出现在空白处，不重叠  
- [ ] 镜头弹簧到新卡，已有卡不单独变小  
- [ ] 只有新卡有 0→1 scale  
- [ ] 不离开画布视角  
- [ ] 再进画布 / 刷新，新卡还在  
- [ ] 侧栏拖入已在画布上的卡：不播出现动画  
- [ ] 滚轮缩放：字和框一起变，PTY 不换行狂跳（仍用 CSS scale 在 world 层 + xterm 用 clientWidth fit）  
- [ ] 边框拖卡 ≠ 选中 PTY 文本  
- [ ] 右键卡（含 PTY 上）有「关闭并终止」  
- [ ] `pnpm tsc --noEmit`

---

## 4. 明确不抄的部分

| cleancode | CaPilot |
| --- | --- |
| `@xyflow/react` 节点/边 | 继续自研 world 层 |
| WebGL xterm raster scale | WebKitGTK 不稳；继续 canvas 2d + 视口裁剪 |
| 组合/group 弹簧 | 不做 |
| `objectPresence: pending` 整段工作流编舞 | 单卡 create 够用 |
| memo 整个 AppShell | 只 memo `CanvasNodeCard` |

---

## 5. 建议提交切分

1. `fix(canvas): spawn on canvas without leaving the view; persist the new node`（步骤 A+B，无动画也要能看见卡）  
2. `fix(canvas): viewport spring owns .canvas-world transform`（步骤 C）  
3. `feat(canvas): cleancode-style create spring on the new card only`（步骤 D+E）

没有第 1 个提交，不要做弹簧。看不见卡时调动画没有意义。
