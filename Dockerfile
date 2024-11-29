FROM node:latest AS build_env

WORKDIR /app
COPY server/package.json ./
RUN corepack enable
RUN yarn
COPY ./server/ .
RUN yarn build
RUN yarn workspaces focus --all --production

FROM gcr.io/distroless/nodejs22-debian12
COPY --from=build_env /app/ /app/
WORKDIR /app/

ENV NODE_ENV="production"
ENV SERVER_PORT=7856
ENV FORWARDING_USER="forward_user"
ENV OPENED_PORTS="7857,7858,7859"

RUN useradd forward_user

CMD ["dist/server.js"]



#7856