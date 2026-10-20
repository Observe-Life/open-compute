# 踩坑对照表

## 1. 升级报 MIGRATION_FAILED

现象：容器启动后日志只有一行 `instance runtime stopped` + `MIGRATION_FAILED`。

原因：`V14__python_prepared_artifacts.sql` 第一段是硬性守卫：

    CREATE TABLE migration_v14_worker_identity_guard (invalid INTEGER NOT NULL CHECK (invalid = 0));
    INSERT INTO migration_v14_worker_identity_guard (invalid)
    SELECT 1 FROM worker_versions WHERE deleted_at_ms IS NULL LIMIT 1;

只要实例里还有未删除的 Worker 版本，这条 INSERT 就违反 CHECK，migration 主动失败。
这是官方设计的 breaking 保护，不是 bug。

处理：不要删数据绕过去。保留旧目录，新建空目录重新初始化，重新部署 Worker。

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

现象：`TARGET_INVALID: target API URL must use HTTPS except for loopbach`。

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
