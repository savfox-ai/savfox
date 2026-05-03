# Gateway Web 构建与发布

这个文档定义 Dioxus 前端与 gateway 静态资源之间的责任边界。

## Source of truth

- 源码位于 `crates/gateway-dioxus`
- browser/backend 共享类型位于 `crates/gateway-shared`
- 可部署静态资源同步到 `crates/gateway-server/static`
- `scripts/build-web.ps1` 是唯一的标准构建与同步入口

## 本地流程

### 一次性构建

- `just web-build`
- `just web-build-release`

### 完整 gateway 开发循环

- `just gateway`
- `just gateway-release`

### 前后端分离开发循环

- 终端 1：`just gateway-frontend`
- 终端 2：`just gateway-backend`

兼容别名仍然保留：
- `just web-serve`
- `just gateway-skip-web`

## 责任边界

不要把 `crates/gateway-server/static` 当作手工维护的源码目录。它是由同步脚本管理的构建产物目标目录。

## CI 期望

CI 应单独验证前端构建是否成功，而不是完全依赖 native gateway 测试来侧面发现问题。
