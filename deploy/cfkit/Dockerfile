# 必须用 open-compute 自己的镜像做基础，原因见 TROUBLESHOOTING.md 第 2、3 条
FROM ghcr.io/elliothux/open-compute:0.3.0

USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates gnupg git bash \
 && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g \
      cf@1.0.0-beta.12 \
      vite \
      typescript \
      @cloudflare/vite-plugin@beta \
    && npm cache clean --force

# ocd 在基础镜像里是 /opt/open-compute/ocd，加软链才能在 PATH 里找到
RUN ln -sf /opt/open-compute/ocd /usr/local/bin/ocd

WORKDIR /app
ENV NODE_ENV=development
ENTRYPOINT []
CMD ["sh"]
