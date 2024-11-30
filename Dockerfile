FROM node:23-alpine AS build_env

WORKDIR /app
COPY server/package.json ./
COPY ./server/.yarnrc.yml .
RUN corepack enable
RUN yarn
COPY ./server/ .
RUN yarn build
RUN yarn workspaces focus --all --production

FROM debian AS runner
RUN apt update && apt install -y nodejs openssh-server htop
COPY --from=build_env /app/ /app/
WORKDIR /app/

ENV NODE_ENV="production"
ENV SERVER_PORT=7856
ENV FORWARDING_USER="tunnel"
ENV OPENED_PORTS="7857,7858,7859"

VOLUME /keys/

RUN useradd --system --create-home --shell /usr/sbin/nologin tunnel
RUN usermod tunnel -p ""
RUN mkdir -p /run/sshd

CMD ["node","dist/server.js"]