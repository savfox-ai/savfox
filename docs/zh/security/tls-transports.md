# 出站 TLS 实现

Discord 流式连接采用 Serenity 正式支持的原生 TLS 后端：Linux 使用 OpenSSL，Windows 使用 SChannel，macOS 使用 Secure Transport。企业代理证书应安装到相应平台的信任存储。

该选择移除了 Serenity 0.12 的 Rustls 后端引入的旧 Rustls 0.22 依赖。模型、登录与其它 Savfox HTTP 请求继续使用共享 HTTP 客户端；自定义 CA 的 PEM 解析采用维护中的 `rustls-pki-types`，保留证书选择、受信任证书处理及 CRL 提示行为。

完整 Discord 配置步骤见 [英文频道说明](../../en/channels/discord.md)。
