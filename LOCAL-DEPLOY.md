# 本地部署套件

本分支 `local-deploy` 只放本机部署 open-compute 的文件，不参与上游同步。
`main` 保持与官方 `v0.3.0` 一致，升级只动 `main`。

## 文件

- `deploy/cfkit/Dockerfile` — 构建容器：Node 22 + cf CLI + vite + tsc + ocd
- `deploy/compose.open-compute.yaml` — 单机 compose 参考
- `deploy/tokens.env.example` — 三个 token 模板
- `deploy/TROUBLESHOOTING.md` — 踩坑对照表

## 从零重装

前提：数据目录 `/vol1/docker/open-compute/data`，网络 `backend`。

1) 生成三个互不相同的 48 位十六进制 token，写进 `/vol1/docker/open-compute/tokens.env`，`chmod 600`。

2) 初始化（只在空目录跑一次）：

```bash
docker run --rm -v /vol1/docker/open-compute/data:/var/lib/open-compute \
  --env-file /vol1/docker/open-compute/tokens.env \
  -e OC_PUBLIC_BIND=0.0.0.0:8787 \
  ghcr.io/elliothux/open-compute-init:0.3.0
```

3) 启动并验证：

```bash
docker compose up -d open-compute
docker exec open-compute /opt/open-compute/ocd --version
docker exec open-compute /opt/open-compute/ocd --no-update-check --system status --json
```

## 构建容器 cfkit

```bash
cd deploy/cfkit && docker build -t cfkit:latest .
docker run --rm cfkit:latest sh -c "node -v; cf --version; ocd --version; vite --version; tsc -v"
```

使用（一次性容器，跑完即删）：

```bash
docker run --rm -it --network backend -v /path/to/worker:/app -w /app cfkit:latest
```

## 升级

```bash
tar czf /vol1/docker/open-compute-backup-$(date +%s).tar.gz -C /vol1/docker/open-compute data
docker pull ghcr.io/elliothux/open-compute:0.3.0
docker pull ghcr.io/elliothux/open-compute-init:0.3.0
# 改 compose 的 image tag
docker compose up -d open-compute
```

实例里已有 Worker 时无法原地升级，见 `deploy/TROUBLESHOOTING.md` 第 1 条。

## 回滚

旧数据目录和旧镜像都在时，改 compose 的 image tag 和 volume 指回旧目录即可。
migration 单向，升级过的目录不能给旧版本用。
