# 五合一编译与运行依赖镜像（cfkit）

一个用完即弃的工具箱镜像：把编译、打包、部署 open-compute Worker 需要的工具全部装在一起，宿主机上不装任何 Node / Rust 依赖。

## 成员

| 工具 | 版本 | 作用 |
|---|---|---|
| Node.js | 22.23.3 | 运行 vite / cf |
| cf | 1.0.0-beta.12 | Cloudflare CLI，负责 build / deploy |
| vite | 8.3.4 | 打包 Worker |
| typescript | 7.0.2 | 类型检查 |
| ocd | 0.3.0 | target 注册与本地部署入口 |

基础镜像用 `ghcr.io/elliothux/open-compute:0.3.0`（Ubuntu 24.04）：ocd 是 glibc 动态链接，Alpine 的 musl 跑不起来，Debian 12 的 libstdc++ 又低于 `GLIBCXX_3.4.31`。

## 构建

```bash
cd deploy/cfkit
docker build -t cfkit:latest .
```

验证：

```bash
docker run --rm cfkit:latest sh -c 'node -v; cf --version; ocd --version; vite --version; tsc -v'
```

## 使用：拉起临时容器，用完即删

它不是常驻服务。每次编译拉起一个容器，`--rm` 让它在退出时自动销毁：

```bash
docker run --rm \
  -v /vol1/docker/open-compute/projects/multimedia-pipeline:/app \
  -w /app cfkit:latest sh -c 'cf build --mode production'
```

资源占用：

- 不使用时：**没有容器进程，不占内存、不占端口、不参与开机自启**，只占磁盘上的镜像层（约 1.27 GB）
- 运行时：编译期间占用 CPU 和内存（vite / tsc 约几百 MB），命令结束容器销毁，内存立即归还

如果嫌每次冷启动慢，可以让容器常驻（基础占用约 10 MB，Node 不启动）：

```bash
docker run -d --name cfkit-idle open-compute:0.3.0 sleep infinity
docker exec -w /vol1/docker/open-compute/projects/multimedia-pipeline cfkit-idle \
  sh -c 'cf build --mode production'
docker rm -f cfkit-idle   # 不用时立刻回到"只剩镜像"的状态
```

## 部署到 open-compute

`ocd` 只允讹 loopback 走 HTTP，所以部署容器要共享 open-compute 的网络命名空间：

```bash
docker run --rm --network container:open-compute \
  -v /vol1/docker/open-compute/projects/multimedia-pipeline:/app \
  -v /vol1/docker/open-compute/tokens.env:/tokens.env:ro \
  -w /app cfkit:latest sh -c '
    mkdir -p /root/.open-compute && chmod 700 /root/.open-compute
    G=$(npm root -g)
    mkdir -p node_modules/@cloudflare node_modules/.bin
    ln -sfn $G/vite node_modules/vite
    ln -sfn $G/@cloudflare/vite-plugin node_modules/@cloudflare/vite-plugin
    ln -sfn $G/typescript node_modules/typescript
    ln -sfn $G/cf node_modules/cf
    ln -sfn $(command -v cf) node_modules/.bin/cf
    cf build --mode production
    D=$(grep DEPLOYER /tokens.env | cut -d= -f2)
    printf "%s" "$D" > /tmp/deployer.token && chmod 600 /tmp/deployer.token
    ocd target add local --api-base-url http://127.0.0.1:8787/client/v4 \
      --instance-id <实例ID> --token-file /tmp/deployer.token
    ocd cf --target local deploy --prebuilt --mode production
  '
```

项目里已经有现成脚本：`/vol1/docker/open-compute/projects/multimedia-pipeline/deploy.sh`。

## 必须知道的几个点

1. **不要往宿主机装 Node / Rust**：所有编译都在容器里完成。
2. **全局包要软链进项目**：cfkit 里没有项目级 `node_modules`，需要把 `vite`、`@cloudflare/vite-plugin`、`typescript`、`cf` 软链进去，并额外建 `node_modules/.bin/cf`，否则报 `CF_INVALID: project-local Cf is missing`。
3. **ocd 需要 `~/.open-compute` 目录**：不存在会报 `TARGET_REGISTRY_INVALID: target registry directory could not be inspected`，先 `mkdir -p /root/.open-compute && chmod 700`。
4. **token 文件要求 owner-only**：属主必须是当前用户、权限 0600。
5. **镜像与容器是两回事**：删容器只释放运行时内存；想连镜像一起清掉用 `docker rmi cfkit:latest`，下次重新 `docker build` 即可。
6. **升级 open-compute 时要重建**：基础镜像 tag 变了，cfkit 也跟着重建，保证 `ocd` 版本与目标实例一致。

## 常用命令

```bash
# 构建
cd deploy/cfkit && docker build -t cfkit:latest .

# 验证成员
docker run --rm cfkit:latest sh -c 'node -v; cf --version; ocd --version; vite --version; tsc -v'

# 一次性编译
docker run --rm -v <项目目录>:/app -w /app cfkit:latest sh -c 'cf build --mode production'

# 看占用
docker images cfkit
docker ps -a | grep cfkit

# 彻底清理
docker rmi cfkit:latest
```
