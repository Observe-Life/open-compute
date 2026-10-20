# 本地部署套件

本分支 `local-deploy` 只放本机部署 open-compute 的文件，不参与上游同步。
`main` 保持与官方 `v0.3.0` 一致，升级只动 `main`。

## 文件

- `deploy/cfkit/Dockerfile` — 构建容器：Node 22 + cf CLI + vite + tsc + ocd
- `deploy/compose.open-compute.yaml` — 单机 compose 参考（与官方一致，含只读、tmpfs、健康检查、停止宽限）
- `deploy/tokens.env.example` — 三个 token 模板
- `deploy/TROUBLESHOOTING.md` — 踩坑对照表

## 从零重装

前提：数据目录 `/vol1/docker/open-compute/data`，网络 `backend`。

0）先停掉占用该目录的旧容器，否则 init 会报 `INSTANCE_REGISTRY_INVALID`：

```bash
docker stop open-compute 2>/dev/null || true
```

1）生成三个互不相同的 48 位十六进制 token，写进 `/vol1/docker/open-compute/tokens.env`，`chmod 600`。
`admin` token 建议沿用旧值，否则统一登录网关里封存的凭据也要重新生成。

2）初始化（只在空目录跑一次）：

```bash
docker run --rm -v /vol1/docker/open-compute/data:/var/lib/open-compute \
  --env-file /vol1/docker/open-compute/tokens.env \
  -e OC_PUBLIC_BIND=0.0.0.0:8787 \
  -e OC_PUBLIC_PORT=8787 \
  -e OC_STORAGE_BACKEND=local \
  ghcr.io/elliothux/open-compute-init:0.3.0
```

3）启动并验证：

```bash
docker compose up -d open-compute
docker exec open-compute /opt/open-compute/ocd --version
docker exec open-compute /opt/open-compute/ocd --no-update-check --system instances --json
docker exec open-compute sh -c 'curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8787/health/live'
docker exec open-compute sh -c 'curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8787/operator/'
```

前两条期望正常输出；后两条期望 `200`。只验 `ocd --version` 和 `--system status` 不够：面板坏掉时这两条也是绿的。

## 控制面板打不开（`/operator/` 返回 503）

### 现象

```
GET /operator/  ->  503
日志            dashboard bootstrap failed  code=RUNTIME_UNAVAILABLE
```

数据库里能看到「版本建好了，但部署没落库」：

```
worker_versions                      1 条 ready（113 个资源文件）
worker_deployments                   0 条
deployment_runtime_assessments       0 条
workers.active_deployment_id         NULL
system_owned_versions.assets_sha256  32 个 0
```

### 原因

0.3.0 首次启动时，面板 bootstrap 在 `wait_for_supervisor_running` 之后立刻执行，会和 workerd 的 generation 握手竞争。竞争失败后版本存在、部署记录没有、指纹写不上，于是**每次启动都重复同一次失败**。

上游 issue：https://github.com/elliothux/open-compute/issues/168

### 修复

**方法一：换成带重试的镜像（已验证可用）**

```yaml
# deploy/compose.open-compute.yaml
image: ghcr.io/observe-life/open-compute:0.3.0-retry1
```

```bash
docker pull ghcr.io/observe-life/open-compute:0.3.0-retry1
docker compose up -d open-compute
docker exec open-compute sh -c 'curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8787/operator/'
# 期望 200
```

该镜像只做两件事：bootstrap 失败后重试 2 次（间隔 2 秒）；失败时把 `error.message()` 打进日志。
tag 与官方 `0.3.0` 隔离，出问题把 tag 改回 `0.3.0` 即可。

**方法二：看日志里的 message 再判断**

换镜像后仍然 503 时，日志会多出 `message = ...`：

- `runtime generation is unavailable before deployment admission` — generation 还没就绪
- `runtime generation changed during deployment admission` — 校验期间 workerd 重启
- `real runtime validation failed internally` / `reported a non-runnable version` — workerd 拒绝该版本

把这三类信息追加到 issue #168，比一句「503」有用得多。

**不要做的事：手工改数据库**

已验证的失败路径：

1. 只插 `worker_deployments` → 触发器拦：`deployment target must be a ready version`
2. 再插 `deployment_runtime_assessments`（state=dispatchable）→ 还必须给 `startup_id`
3. 再更新 `workers.active_deployment_id` → 三者同时满足才放行
4. 全部补齐 → 错误从 `RUNTIME_UNAVAILABLE` 变成 `INTERNAL`，仍然 503
5. 中途误动对象存储 → 实例可能进入 `OBJECT_STORAGE_INTEGRITY_ERROR` / `MIGRATION_FAILED`

面板正常后必须做的一步：把登录网关的实例 ID 改成当前实例，见下一节。

## 实例 ID 与统一登录网关

每次重新初始化，实例 ID 都会变。登录网关的 `state.json` 里保存着旧 ID 和用它封存的凭据，不同步就会出现「登录成功但面板连不上」。

查当前实例 ID：

```bash
docker exec open-compute /opt/open-compute/ocd --no-update-check --system instances --json
```

更新网关（在网关容器内执行，用 master.key 重新封存当前 admin token）：

```bash
docker exec container-login-gateway node -e '
const fs=require("fs"),crypto=require("crypto");
const key=fs.readFileSync("/secrets/master.key");
const p="/data/state.json";
const st=JSON.parse(fs.readFileSync(p,"utf8"));
const token=fs.readFileSync("/run/secrets/oc-token","utf8").trim();
const seal=(v,k)=>{const iv=crypto.randomBytes(12),c=crypto.createCipheriv("aes-256-gcm",k,iv),d=Buffer.concat([c.update(v,"utf8"),c.final()]);return [iv,c.getAuthTag(),d].map(x=>x.toString("base64")).join(".");};
const app=st.apps.find(a=>a.id==="compute");
app.instance="<新的实例ID>";
app.credential=seal(token,key);
fs.writeFileSync(p+".tmp",JSON.stringify(st),{mode:0o600});
fs.renameSync(p+".tmp",p);
console.log("updated", app.instance);
'
docker restart container-login-gateway
```

验证（不需要知道登录密码）：

```bash
docker exec container-login-gateway node -e '
const fs=require("fs"),crypto=require("crypto");
const key=fs.readFileSync("/secrets/master.key");
const st=JSON.parse(fs.readFileSync("/data/state.json","utf8"));
const app=st.apps.find(a=>a.id==="compute");
const unseal=(v,k)=>{const [iv,tag,d]=v.split(".").map(x=>Buffer.from(x,"base64"));const c=crypto.createDecipheriv("aes-256-gcm",k,iv);c.setAuthTag(tag);return Buffer.concat([c.update(d),c.final()]).toString("utf8");};
(async()=>{
const token=unseal(app.credential,key);
const r=await fetch("http://open-compute:8787/operator/session",{method:"POST",headers:{Authorization:"Bearer "+token,"Content-Type":"application/json","Sec-Fetch-Site":"same-origin"},body:"{}"});
const s=await r.json();
const r2=await fetch("http://open-compute:8787/client/v4/accounts/"+app.instance,{headers:{Authorization:"Bearer "+s.session_token}});
console.log("session",r.status,"instance_api",r2.status);
})();
'
```

期望输出：`session 200 instance_api 200`。

## 构建容器 cfkit

```bash
cd deploy/cfkit && docker build -t cfkit:latest .
docker run --rm cfkit:latest sh -c 'node -v; cf --version; ocd --version; vite --version; tsc -v'
```

使用（一次性容器，跑完即删）：

```bash
docker run --rm -it --network backend -v /path/to/worker:/app -w /app cfkit:latest
```

## 升级

**实例里有 Worker 版本时不能原地升级。** V14 迁移的第一段是硬性守卫：

```sql
INSERT INTO migration_v14_worker_identity_guard (invalid)
SELECT 1 FROM worker_versions WHERE deleted_at_ms IS NULL LIMIT 1;
```

只要还有未删除的 Worker 版本，这条 INSERT 就违反 CHECK，迁移主动失败，日志是 `MIGRATION_FAILED`。

正确顺序：

1. 备份旧数据目录（整个 `data`，或至少 `keys/` + `instances/default/`）
2. 停旧容器
3. 新建空目录，用新版本 init 镜像初始化
4. 启动新容器，按「从零重装」的验证清单检查（**必须包含 `/operator/`**）
5. 重新部署 Worker
6. 按「实例 ID 与统一登录网关」一节更新网关

即「新建实例 + 迁移数据」，不是改 tag 重启。

## 回滚

旧数据目录和旧镜像都在时，改 compose 的 image tag 和 volume 指回旧目录即可。
migration 单向，升级过的目录不能给旧版本用。
