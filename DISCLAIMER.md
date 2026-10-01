# 免责声明

> 本文件同时提供[中文版](#中文版)与 [English version](#english-version)。两个版本如有歧义，以中文版为准。
> This file is provided in [Chinese](#中文版) and [English](#english-version). If the two versions conflict, the Chinese version prevails.

---

# 中文版

## 一、总则与适用

1. 本声明适用于 **speed** 项目（以下简称"本项目"）的**全部**源代码、构建产物（APK）、文档、规则源引用，以及任何随附材料。
2. 本项目以 **GNU General Public License v3.0** 发布，全文见 [LICENSE](LICENSE)。本声明是对 GPL-3.0 中免责条款（第 15、16 条）的**具体化与补充**，不取代它。
3. **下载、安装、启动或以任何方式使用本项目，即视为你已完整阅读、理解并同意本声明的全部条款。** 若你不同意其中任何一条，请**立即停止使用并卸载**。

## 二、软件性质与定位

1. 本项目是一个**免 Root 的用户态网络工具**。它在设备本地建立一个 VPN 隧道，按规则表为每个连接选择网络出口**地址**。
2. 本项目**不是** VPN 服务提供者，**不是**代理服务商，**不运营任何服务器**，**不销售任何网络服务**，也**不提供任何网络出口**。
3. 本项目**不改变你所处的网络环境**。它换的是"连到哪个地址"，不是"从哪个国家出去"。
4. 本项目**不解密 TLS**。它不代理、不记录、不修改任何加密内容，也无法读取你的任何加密流量。
5. 本项目**不做任何"翻墙"承诺**，也不保证能够访问任何被限制的资源。能否访问某个站点，取决于该站点地址是否可达、以及该地址的证书是否覆盖该域名——这两件事都不在本项目的控制之内。

## 三、无担保：本项目按"现状"提供

1. 本项目按 **"现状"（AS IS）** 与 **"现有"（AS AVAILABLE）** 提供，**不附带任何明示或默示的担保**，包括但不限于：适销性、特定用途适用性、不侵权、无病毒、无错误、不中断、结果准确。
2. 我们不担保：
   - 能访问任何**特定**网站、域名、服务或资源；
   - 网络连通性、速度、延迟、稳定性或可用性达到**任何**标准；
   - 规则表中的地址始终**正确、有效或安全**；
   - 与你的设备、系统版本、厂商定制 ROM 或其他应用**兼容**；
   - 数据不丢失、设备不损坏、不产生额外流量费用；
   - 任何缺陷会被修复，或会发布任何后续版本。

## 四、使用合规责任（完全由你自负）

1. **你须自行确保你的使用行为符合你所在国家/地区及所在地的全部适用法律法规。**
2. 本项目的**唯一目的是提升网络访问的可靠性与效率**。我们**不鼓励、不支持、不协助**任何人利用本项目从事违反任何适用法律的行为。
3. 一切因使用本项目而产生的法律后果、行政处罚、账号封禁、合同违约、第三方索赔，**均由你自行承担**，与本项目的作者及贡献者无关。
4. 我们**不提供**任何规避法律监管的指导、方法或技术支持。任何此类请求都不会被受理。

## 五、功能与效果不保证：技术上的原因

这一节说明为什么"装了但某些站点打不开"是**设计边界**，而不是可以修的缺陷。

1. **规则源提供的是"地址"，不是"可达性"。** 把 IP 写进规则只跳过 DNS 解析，**完全不改变**这个 IP 是否可达。一个正确的地址依然可能连不上。
2. **地址会过期。** 上游服务的 IP 随时变更。过期地址的典型表现是**挂满超时之后报"候选耗尽"**；若该条目只有一个地址，就没有第二条路可走。
3. **存在按名字的干扰。** 有些域名 TCP 能连通、TLS 握手之后即被静默（连上但零字节返回）。对这类域名，**换地址无效，放宽证书校验也无效**——只有换一个网络出口才行，而**本项目不提供出口**。
4. **连通性在分钟尺度上抖动。** 任何"某域名通/不通"的结论都必须带上时间窗；单次结果不构成判据。
5. **不解密 TLS 的后果**：当所有已知地址的证书都不覆盖目标域名时，本项目**无法处理**（这是本项目与"本地中间人解密"类方案的根本区别）。
6. **应用自身流量被刻意排除在隧道之外**（防止自环），因此应用自己的联网请求——例如**检查更新**——走的是**裸网络**。
7. **冷启动存在首包延迟**（数秒量级），这是设计行为而非故障。

## 六、第三方中转与第三方内容（重要）

### 6.1 更新下载经第三方镜像中转

1. 本项目的**检查更新与 APK 下载均通过 `gh-proxy.com` 这一第三方公共服务中转**。这不是偏好，而是可达性要求：应用自身流量被排除在隧道之外（见第五节第 6 条），直连 GitHub 在部分网络环境下不可达。
2. 我们**不运营、不控制、不担保**该服务。它的**可用性、正确性、完整性、日志策略与隐私策略完全不在我们的控制之内**，且可能随时变更或停止服务。
3. 经第三方中转意味着**你的下载请求（包含你的 IP、请求时间、User-Agent 等）会经过该第三方**。
4. **完整性由 Android 的 APK 签名校验兜底**：被篡改的安装包签名不匹配，系统会拒绝安装或覆盖。但**这不能保证第三方不记录、不分析你的下载行为**。
5. 若你不接受经第三方中转，请**从源码自行构建**（见 README 的"从源码构建"一节），或自行通过你信任的渠道获取安装包。
6. 中转的**具体地址可能随版本变化**，本声明不承诺其永久有效。

### 6.2 第三方规则源

1. 默认规则源来自 **HelloGitHub（GitHub520，`521xueweihan/GitHub520`）**，其许可为 **CC BY-NC-ND 4.0**：**禁止商业使用、禁止演绎**。
2. 可选备用规则源来自 **`maxiaof/github-hosts`**。
3. 本项目**仅在运行期按 URL 引用**这些文档，**不将其内容打包进 APK**。文档的著作权归其各自作者所有，**使用时请遵守其各自的许可条款**。
4. **两份文档的许可不同，其镜像列表不得混用。**
5. 规则源可能包含**过期、错误、或不再属于原所有者的地址**。我们不对其准确性、及时性或合法性作任何担保。
6. 规则源的服务器可能**到期、停服或变更**（已知：HelloGitHub 服务器计划于 **2026-12-31** 到期）。这会导致默认源不可用，需要你自行更换。

## 七、数据与隐私

1. 本项目**不收集、不上传**你的网络流量内容、浏览记录、域名访问列表、设备标识或任何个人信息。
2. 本项目**不内置任何统计、分析、广告或崩溃上报 SDK**。
3. 运行日志保存在**设备本地**，你可在应用内查看与清除。
4. 规则缓存与设置保存在**应用私有目录**，随应用卸载而删除（`adb uninstall` 或系统卸载会清除应用数据）。
5. **例外（必须知悉）**：
   - **检查更新**会向第三方镜像服务发起请求（见第六节）；
   - **拉取规则**会向你配置的规则源发起请求；
   - 你的**普通网络请求**会经由你选择的网络出口地址所属的第三方，其隐私策略**由该第三方决定**，与本项目无关。
6. **VPN 权限说明**：本项目需要系统 VPN 授权以在**设备本地**建立隧道。该授权不意味着你的数据被传送到本项目的任何服务器——本项目**没有**服务器。

## 八、无技术支持、无服务等级承诺

1. 本项目由维护者**在业余时间**开发与维护，**不提供任何形式的技术支持、服务等级协议（SLA）、可用性承诺或响应时限**。
2. **不保证**任何 issue、邮件或反馈会得到回复、修复或采纳。
3. **不保证**提供后续版本、安全更新、兼容性维护或迁移方案。
4. 本项目可能**随时被修改、暂停或终止**，恕不另行通知。

## 九、责任限制

1. 在适用法律允许的最大范围内，作者与贡献者**对任何直接、间接、附带、特殊、惩罚性或后果性损害不承担责任**，包括但不限于：
   - 数据丢失、设备损坏、系统不稳定或无法启动；
   - 网络中断、服务不可用、业务中断、利润损失；
   - 流量费用、漫游费用、电费；
   - 第三方账号被封禁、限制、验证或索赔；
   - 因规则源地址过期、错误、被篡改或规则源停服导致的一切后果；
   - 因第三方镜像服务导致的一切后果；
   - 因误用应用内控制台（开发者视图）导致的一切后果。
2. **即使作者已被告知上述损害的可能性，本限制依然适用。**
3. 若你所在司法辖区不允许排除某些担保或限制某些责任，则上述部分条款可能**不适用于你**；此时其余条款仍然**完全有效**。

## 十、你自担的风险清单

使用本项目，意味着你明确接受以下**具体**风险：

1. 网络可能在运行期间中断，需断开隧道才能恢复。
2. 隧道建立初期可能有**数秒**延迟（冷启动首包延迟），这是设计行为。
3. 某些应用可能因网络路径变化而**登录失效、触发风控或频繁要求验证**。
4. 部分网站可能因**地址不可达、地址过期或按名字干扰**而无法访问，且本项目**无法修复**。
5. 规则表可能包含**过期、错误或不再属于原所有者**的地址。
6. **电池消耗**可能增加。
7. **系统更新或厂商定制 ROM** 可能改变 VPN 行为，导致本项目失效。
8. **卸载应用会清除应用数据**（含规则缓存与全部设置），设置需重新配置或从备份恢复。
9. 使用 **proxy 模式**时，本项目**不读取任何内核设置文档**，其行为与 VPN 模式不同（该模式按设计不对外开放）。
10. 应用内控制台在**开发者视图**下可用，**误操作可能导致网络不可用**。

## 十一、出口管制与制裁

1. 你须自行确保**不**将本项目用于受出口管制、经济制裁或类似法律限制的国家、地区、实体或个人。
2. 我们**不保证**本项目在任何特定司法辖区的合法性、可用性或可获取性。

## 十二、商标与第三方权利

1. **GitHub、Android、Google** 等名称与标识为其各自所有者的商标。本项目与它们**无任何隶属、赞助、认可或背书关系**。
2. 本项目引用的所有第三方项目、文档与规则源，其著作权归**其各自作者**所有。
3. 本项目的名称与图标仅用于标识本项目本身。

## 十三、声明变更

1. 我们保留**随时修改本声明**的权利，恕不另行通知。
2. 修改后的声明自**提交至本仓库之时**起生效。
3. **在声明修改后继续使用本项目，即视为你接受修改后的声明。** 建议定期回看本文件。

## 十四、可分性

若本声明的任何条款被认定为**无效或不可执行**，该条款应在**最小必要范围内**被限制或删除，其余条款**继续完全有效**。

## 十五、接受

**你下载、安装、启动或使用本项目，即表示你已阅读、理解并同意本声明的全部内容。**

---

# English version

> This is a courtesy translation. **If it conflicts with the Chinese version above, the Chinese version prevails.**

## 1. Scope

1. This notice applies to **all** source code, build artifacts (APKs), documentation, rule-source references and any accompanying material of the **speed** project (the "Project").
2. The Project is released under the **GNU General Public License v3.0**; see [LICENSE](LICENSE). This notice **particularises and supplements** the warranty disclaimers in GPL-3.0 sections 15 and 16. It does not replace them.
3. **By downloading, installing, launching or otherwise using the Project you confirm that you have read, understood and accepted this notice in full.** If you do not accept any part of it, **stop using the Project and uninstall it immediately**.

## 2. What the Project is, and what it is not

1. The Project is a **root-free userspace networking tool**. It creates a local VPN tunnel on your device and selects an egress **address** per connection according to a rule table.
2. The Project is **not** a VPN provider, **not** a proxy service, **operates no servers**, **sells no network service**, and **provides no network egress of any kind**.
3. The Project **does not change the network you are on**. It changes *which address* a connection is sent to, not *which country it leaves from*.
4. The Project **does not decrypt TLS**. It does not proxy, log or modify encrypted content, and it cannot read your encrypted traffic.
5. The Project makes **no promise of circumventing anything**, and does not guarantee access to any restricted resource. Whether a site is reachable depends on whether its addresses are reachable and whether the certificate on those addresses covers the name — neither of which the Project controls.

## 3. No warranty: provided "as is"

1. The Project is provided **"AS IS"** and **"AS AVAILABLE"**, **without warranty of any kind**, express or implied, including but not limited to merchantability, fitness for a particular purpose, non-infringement, freedom from viruses, freedom from errors, uninterrupted operation, or accuracy of results.
2. We do **not** warrant that:
   - any **specific** site, domain, service or resource will be reachable;
   - connectivity, speed, latency, stability or availability will meet **any** standard;
   - addresses in the rule table are **correct, current or safe**;
   - the Project is **compatible** with your device, OS version, vendor ROM or other apps;
   - data will not be lost, the device will not be damaged, or no extra traffic charges will be incurred;
   - any defect will be fixed, or that any future release will be published.

## 4. Compliance is entirely your responsibility

1. **You must ensure that your use complies with every law and regulation applicable to you and to your location.**
2. The Project's **sole purpose is to improve the reliability and efficiency of network access**. We **do not encourage, support or assist** anyone in using it to violate any applicable law.
3. All legal consequences, administrative penalties, account bans, contractual breaches and third-party claims arising from your use of the Project are **borne by you alone**, and are unrelated to the Project's authors and contributors.
4. We **provide no** guidance, method or technical support for evading legal regulation. No such request will be entertained.

## 5. Why results are not guaranteed (the technical reasons)

This section explains why "it installed, but some sites do not load" is a **design boundary**, not a fixable defect.

1. **A rule source supplies an *address*, not *reachability*.** Writing an IP into a rule only skips DNS resolution; it does **not** change whether that IP is reachable. A correct address can still fail to connect.
2. **Addresses go stale.** Upstream IPs change without notice. The typical symptom is **hanging until a timeout, then reporting "candidates exhausted"**; if the entry has only one address, there is no second path.
3. **Per-name interference exists.** Some domains complete a TCP handshake and are then silently dropped after the TLS handshake (connected, zero bytes back). For these, **changing the address does not help and relaxing certificate validation does not help** — only a different egress would, and **the Project provides no egress**.
4. **Connectivity fluctuates on a minute scale.** Any "this domain works / does not work" conclusion carries a time window; a single sample is not evidence.
5. **Because TLS is not decrypted**, the Project **cannot** handle the case where no known address carries a certificate covering the target name. This is the fundamental difference from local man-in-the-middle designs.
6. **The app's own traffic is deliberately excluded from its tunnel** (to prevent loops), so the app's own requests — such as **the update check** — use the **raw network**.
7. **A cold start carries a first-packet delay** (on the order of seconds). This is by design, not a fault.

## 6. Third-party relay and third-party content (important)

### 6.1 Update downloads are relayed through a third-party mirror

1. **Both the update check and the APK download go through `gh-proxy.com`, a third-party public service.** This is a reachability requirement rather than a preference: the app's own traffic is excluded from the tunnel (see §5.6), and GitHub is not directly reachable from some networks.
2. We **do not operate, control or warrant** that service. Its **availability, correctness, integrity, logging and privacy practices are entirely outside our control**, and it may change or shut down at any time.
3. Relaying means **your download request — including your IP, request time and User-Agent — passes through that third party**.
4. **Integrity is backstopped by Android's APK signature verification**: a tampered package will not match the signature and will be refused. **This does not guarantee that the third party does not record or analyse your download.**
5. If you do not accept third-party relaying, **build from source** (see the README) or obtain the package through a channel you trust.
6. The **specific relay address may change between versions**; this notice does not promise it is permanent.

### 6.2 Third-party rule sources

1. The default rule source is **HelloGitHub (GitHub520, `521xueweihan/GitHub520`)**, licensed **CC BY-NC-ND 4.0**: **no commercial use, no derivatives**.
2. An optional secondary source is **`maxiaof/github-hosts`**.
3. The Project **references these documents by URL at runtime only** and **does not bundle their contents into the APK**. Copyright remains with their respective authors; **comply with their licences when you use them**.
4. **The two documents carry different licences; their mirror lists must not be mixed.**
5. Rule sources may contain **stale, wrong, or no-longer-owned addresses**. We warrant nothing about their accuracy, currency or legality.
6. Rule-source servers may **expire, shut down or change** (known: the HelloGitHub server is scheduled to expire on **2026-12-31**), which will make the default source unusable until you change it.

## 7. Data and privacy

1. The Project **does not collect or upload** your traffic content, browsing history, visited-domain list, device identifiers or any personal information.
2. The Project **bundles no analytics, advertising or crash-reporting SDK**.
3. Runtime logs are kept **on your device** and can be viewed and cleared in the app.
4. The rule cache and settings live in the app's **private storage** and are removed when the app is uninstalled (`adb uninstall` or a system uninstall clears app data).
5. **Exceptions you must know about:**
   - the **update check** contacts a third-party mirror (§6.1);
   - **fetching rules** contacts the rule source you configured;
   - your **ordinary network requests** pass through the third party that owns your chosen egress address; their privacy practices are **theirs**, not ours.
6. **About the VPN permission**: the Project needs system VPN authorisation to create a tunnel **on your device**. That authorisation does **not** mean your data is sent to any server of ours — the Project **has no servers**.

## 8. No support, no service level

1. The Project is developed and maintained **in the maintainer's spare time**, with **no technical support, no service-level agreement, no availability commitment and no response-time commitment**.
2. There is **no guarantee** that any issue, email or feedback will be answered, fixed or accepted.
3. There is **no guarantee** of future versions, security updates, compatibility maintenance or migration paths.
4. The Project may be **modified, suspended or discontinued at any time** without notice.

## 9. Limitation of liability

1. To the maximum extent permitted by applicable law, the authors and contributors are **not liable for any direct, indirect, incidental, special, punitive or consequential damages**, including but not limited to:
   - data loss, device damage, system instability or failure to boot;
   - network outages, unavailable services, business interruption, lost profits;
   - traffic charges, roaming charges, electricity costs;
   - third-party account bans, restrictions, challenges or claims;
   - anything arising from stale, wrong or tampered rule-source addresses, or from a rule source shutting down;
   - anything arising from the third-party mirror service;
   - anything arising from misuse of the in-app console (developer view).
2. **This limitation applies even if the authors have been advised of the possibility of such damages.**
3. If your jurisdiction does not allow the exclusion of certain warranties or the limitation of certain liabilities, some clauses above may **not apply to you**; the remaining clauses stay **fully in force**.

## 10. Specific risks you accept

By using the Project you explicitly accept the following **concrete** risks:

1. The network may drop while the tunnel is up; disconnecting the tunnel is needed to recover.
2. Tunnel establishment may take **several seconds** (cold-start first-packet delay). This is by design.
3. Some apps may suffer **lost sessions, risk-control triggers or repeated verification** because their network path changed.
4. Some sites may be unreachable due to **dead addresses, stale addresses or per-name interference**, and the Project **cannot fix this**.
5. The rule table may contain **stale, wrong or no-longer-owned** addresses.
6. **Battery usage** may increase.
7. **OS updates or vendor ROMs** may change VPN behaviour and break the Project.
8. **Uninstalling clears app data** (rule cache and all settings); you will need to reconfigure or restore from a backup.
9. In **proxy mode** the Project **does not read any kernel settings document**, so it behaves differently from VPN mode (that mode is not offered publicly, by design).
10. The in-app console is reachable under **developer view**; **a wrong command can make networking unusable**.

## 11. Export control and sanctions

1. You must ensure you do **not** use the Project in or for any country, region, entity or person subject to export controls, economic sanctions or similar legal restrictions.
2. We **do not warrant** the legality, availability or obtainability of the Project in any particular jurisdiction.

## 12. Trademarks and third-party rights

1. **GitHub, Android, Google** and their marks are trademarks of their respective owners. The Project has **no affiliation with, sponsorship by, or endorsement from** them.
2. All third-party projects, documents and rule sources referenced by the Project remain the copyright of **their respective authors**.
3. The Project's name and icon identify the Project itself and nothing else.

## 13. Changes to this notice

1. We reserve the right to **change this notice at any time** without notice.
2. A revised notice takes effect **when it is committed to this repository**.
3. **Continuing to use the Project after a revision means you accept the revised notice.** Check back periodically.

## 14. Severability

If any clause of this notice is held **invalid or unenforceable**, that clause shall be limited or removed **to the minimum extent necessary**, and the remaining clauses remain **fully in force**.

## 15. Acceptance

**By downloading, installing, launching or using the Project, you confirm that you have read, understood and accepted this notice in full.**
