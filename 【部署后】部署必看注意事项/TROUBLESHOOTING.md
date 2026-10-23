# 踩坑对照表

按「现象 → 原因 → 处理」记录，按遇到顺序编号。新坑往下加。

## 1. 升级报 MIGRATION_FAILED

现象：容器启动后日志只有一行 `instance runtime stopped` + `MIGRATION_FAILED`。

原因：`V14__python_prepared_artifacts.sql` 第一段是硬性守卫：

    CREATE TABLE migration_v14_worker_identity_guard (invalid INTEGER NOT NULL CHECK (invalid = 0));
    INSERT INTO migration_v14_worker_identity_guard (invalid)
    SELECT 1 FROM worker_versions WHERE deleted_at_ms IS NULL LIMIT 1;

只要实例里还有未删除的 Worker 版本，这条 INSERT 就违反 CHECK，migration 主动失败。
这是官方设计的 breaking 保护，不是 bug。

处理：不要删数据绕过去。保留旧目录，新建空目录重新初始化，重新部署 Worker。

**重要**：新实例的 ID 会变。统一登录网关 `state.json` 里的 `instance` 和 `credential` 必须同步更新，否则会出现「登录成功但面板连不上」。步骤见 `README.md` 的「实例 ID 与统一登录网关」一节。

## 2. Alpine 基础镜像里 ocd 报 not found

现象：cfkit 用 `node:22-alpine`，`ocd --version` 报 `not found`，但文件明明存在。

原因：ocd 是 glibc 动态链接的二进制，Alpine 是 musl，找不到 `/lib64/ld-linux-x86-64.so.2`。

处理：用 Debian/Ubuntu 基础镜像。

## 3. Debian bookworm 报 GLIBCXX_3.4.31 not found

现象：`node:22-bookworm-slim` 里 ocd 报
`/lib/x86_64-linux-gnu/libstdc++.so.6: version GLIBCXX_3.4.31 not found`。

原因：bookworm 的 libstdc++ 版本低于 ocd 编译时用的 GCC 13。

处理：直接用 `ghcr.io/elliothux/open-compute:0.3.0` 作基础镜像（Ubuntu 24.04，版本正好匹配）。

## 4. ocd 不在 PATH

现象：cfkit 里 `ocd` 报 not found，但 `/opt/open-compute/ocd` 存在。

处理：`RUN ln -sf /opt/open-compute/ocd /usr/local/bin/ocd`。

## 5. ocd target add 报 must use HTTPS

现象：`TARGET_INVALID: target API URL must use HTTPS except for loopback`。

原因：ocd 只允许 127.0.0.1 走 HTTP，其他地址必须 HTTPS。

处理：`docker run --network container:open-compute`，让构建容器与 daemon 共享网络，用 `http://127.0.0.1:8787/client/v4`。

## 6. ocd target add 报 token file must be owned by current user

现象：`TARGET_INVALID: target token file must be owned by the current user`。

原因：宿主机 token 文件 owner 是 65532，容器内是 root(0)。

处理：进入容器后 `cp` 到容器内路径并 `chmod 600`，用容器内的路径注册。

## 7. 多出一个随机名容器（如 happy_wiles）

现象：`docker ps` 里出现一个随机名容器，镜像和 open-compute 相同。

原因：用 `docker run --rm ... --system run` 做测试，daemon 常驻不退出，`--rm` 不会触发删除。

处理：`docker rm -f <名字>`。以后测试加 `timeout 30` 或测试完立即删。

## 8. init 镜像拒绝初始化

现象：`init-volume: refusing an unmarked nonempty volume`。

原因：目标目录非空但缺少 `.compose-initialized` 标记。

处理：换一个空目录重新初始化；旧目录原样保留用于回滚。

补充：如果报的是 `INSTANCE_REGISTRY_INVALID`，先停掉占用该目录的旧容器（`docker stop open-compute`），再跑 init。

## 9. 面板打不开：/operator/ 返回 503

现象：

    GET /operator/  ->  503
    日志            dashboard bootstrap failed  code=RUNTIME_UNAVAILABLE

原因：首次启动创建面板版本失败后，`control_idempotency` 表里会留下一条记录：

    scope            version.create
    idempotency_key  system-dashboard:<面板指纹>

之后每次启动都会命中这条记录，直接复用上次的结果，不再真正创建面板版本，所以永远 503。

处理：删掉这条记录，重启（原版镜像即可）。

1）备份：

```bash
docker exec open-compute sh -c 'cp /var/lib/open-compute/instances/default/data/control.sqlite /var/lib/open-compute/instances/default/data/control.sqlite.bak'
```

2）删记录（在宿主机执行）：

```bash
python3 - <<'PY'
import sqlite3
p = '/vol1/docker/open-compute/data/instances/default/data/control.sqlite'
con = sqlite3.connect(p)
n = con.execute("""
    delete from control_idempotency
    where scope = 'version.create'
      and idempotency_key like 'system-dashboard:%'
""").rowcount
con.commit()
con.close()
print('deleted', n)
PY
```

3）重启并验证：

```bash
docker restart open-compute
sleep 60
docker exec open-compute sh -c 'curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8787/operator/'
```

期望 `200`。成功后表里会自动写回一条指向新版本的记录。

**注意**：面板正常时不要删这条记录，它指向的是成功版本，删了会让下次启动重新创建。

## 10. 面板正常后登录网关仍连不上

现象：8788 登录成功，但面板空白/报「实例连接失败」。

原因：重新初始化后实例 ID 变了，网关 `state.json` 里还是旧 ID。

处理：按 `README.md` 的「实例 ID 与统一登录网关」一节更新 `instance` 并重新封存 `credential`，然后重启网关容器。

## 11. 重建镜像后 /operator/ 第一次 503，重启即自愈

现象：改 compose 镜像 tag、`docker compose up -d` 重建容器后，`/health/live` 是 200，`/operator/` 是 503。

跟第 9 条的区别：此时 `control_idempotency` 里**并没有** `system-dashboard:*` 记录可删（查出来是 0 条）。

原因：启动时序问题——dashboard 版本创建尝试在运行时还没准备好时进行了一次，失败一次后状态卡住。

处理：直接 `docker restart open-compute`，等约 50 秒，`/operator/` 恢复 200。**不要**按第 9 条删记录（没有记录可删且不需要）。

判断用哪条：503 时先看 `control_idempotency` 有没有 `system-dashboard:*` 记录。**有 → 第 9 条**（删记录）；**没有 → 本条**（重启一次）。

## 12. 触发新实例报 9100006「the request conflicts with current state」

现象：向 `/workflows/<definition>/instances` POST 触发，返回

    {"success":false,"errors":[{"code":9100006,"message":"the request conflicts with current state"}]}

原因分两层：

1. 直接原因：该 workflow definition 在 control 库里被标成 `availability='unavailable'`：
   ```
   workflow_definitions
     availability='unavailable'
     availability_code='WORKFLOW_INVARIANT_VIOLATION'
   ```
   恢复语句：
   ```sql
   UPDATE workflow_definitions
   SET availability='healthy', availability_code=NULL
   WHERE availability='unavailable';
   ```
2. 深层原因：调度的对账（reconcile）每 30 秒 probe 一次工作流实例（见【部署前】独立维护改动清单 P001 的熔断与 P002 的窗口冲突），发现异常会把 definition 打成 unavailable。根因在调度内核的对账与重试窗口冲突——已经单独修复（见【部署前】P001），若再次出现，先看【部署前】清单里最新一条的「为什么这会发生」。

SQL 合法值：`availability` ∈ ('healthy','degraded','unavailable')，且约束要求 healthy 时 `availability_code` 必须为 NULL。备份先行：
```bash
docker exec open-compute sh -c 'cp /var/lib/open-compute/instances/default/data/control.sqlite /var/lib/open-compute/instances/default/data/control.sqlite.bak'
```

## 13. Worker 步骤带重试参数报 WORKFLOW_STEP_CONFIG_UNSUPPORTED

现象：workflow step 写法 `step.do("name", { retries: { maxAttempts: 3 } }, fn)`，实例报
`WORKFLOW_STEP_CONFIG_UNSUPPORTED` 后直接 errored。

原因：本分支的 workerd workflow 步骤不支持 retries 配置参数。

处理：去掉步骤级重试配置；需要重试时在队列消费者层做（`maxRetries` + `message.retry()`），或把重试逻辑写进业务代码。

## 14. 部署后第一次调用报 RUNTIME_UNAVAILABLE / 503（30 秒后失败）

现象：`cf deploy` 完成后立刻调用 Worker，30 秒超时返回
`RUNTIME_UNAVAILABLE: the workerd runtime is unavailable`。

原因：部署切换的静默窗口期，旧世代退避、新世代未就绪。

处理：等 30–60 秒重试即可自愈（P001 的熔断冷却兜底）。**不要**按第 9 条删记录。

## 15. 附件经数据通道转写报 gemini INVALID_ARGUMENT / 未读出内容

现象：astrbot-kb-sync 的图片转写全部「未读出内容」，gemini 返回
`400 INVALID_ARGUMENT: Unable to process input image`。

原因：数据通道 `GET /api/attachment/<id>` 返回了 `{mime, bytes}` 的 JSON 包装，
消费方把 JSON 文本当图片字节转写。

处理：通道返回**纯二进制流**（mime 放 `x-attachment-mime` 响应头），消费方按
`arrayBuffer()` 读取、从头字段取 mime。修复于 2026-10-24。

另注：转写报 `gemini 429` 是免费层当日配额耗尽（按天重置），不是代码问题。
