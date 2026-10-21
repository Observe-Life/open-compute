# 本地部署

本目录只放**部署后**的东西：拿到现成镜像之后，怎么装、怎么验、出问题怎么查。

镜像本身是怎么产出的，不在这里，见根目录 [`独立维护改动清单.md`](../独立维护改动清单.md)。

## 文件

- `deploy/cfkit/` — 五合一构建容器（Node 22 + cf CLI + vite + tsc + ocd），详见 `deploy/cfkit/README.md`
- `deploy/compose.open-compute.yaml` — 单机 compose 参考（只读根、tmpfs、健康检查、停止宽限）
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
  ghcr.io/observe-life/open-compute-init:0.3.0-stallfix1
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

完整步骤见 `deploy/TROUBLESHOOTING.md` 第 9 条。要点：`control_idempotency` 里残留一条 `system-dashboard:*` 记录，删掉重启即可；面板正常时不要删。

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

见 `deploy/cfkit/README.md`。

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

## 我们的镜像

| 用途 | 镜像 |
|---|---|
| 运行时 | `ghcr.io/observe-life/open-compute:0.3.0-stallfix1` |
| 初始化 | `ghcr.io/observe-life/open-compute-init:0.3.0-stallfix1` |
| 上游原版（回滚用） | `ghcr.io/elliothux/open-compute:0.3.0` |
