FROM node:20-slim AS builder
WORKDIR /app
COPY package.json pnpm-workspace.yaml ./
COPY miniapp/package.json ./miniapp/
COPY shared/package.json ./shared/
RUN npm install -g pnpm && pnpm install --filter miniapp...
COPY miniapp ./miniapp
COPY shared ./shared
RUN pnpm --filter miniapp build

FROM nginx:1.25-alpine
COPY --from=builder /app/miniapp/dist /usr/share/nginx/html
EXPOSE 80
CMD ["nginx", "-g", "daemon off;"]
