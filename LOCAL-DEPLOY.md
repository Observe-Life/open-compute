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

### 原因

首次启动创建面板版本失败后，`control_idempotency` 表里会留下一条记录：

```
scope            version.create
idempotency_key  system-dashboard:<面板指纹>
```

之后每次启动都会命中这条记录，直接复用上次的结果，不再真正创建面板版本，所以永远 503。

### 修复：删掉这条记录，重启（原版镜像即可）

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

### 注意

面板正常时**不要删**这条记录：它指向的是成功版本，删了会让下次启动重新创建。

### 面板正常后

把登录网关的实例 ID 改成当前实例，见下一节。

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
