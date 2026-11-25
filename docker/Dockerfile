# SpoolGuard en modo servidor: CUPS mínimo (colas RAW, sin filtros ni interfaz web) + control-server.
# Construir desde la raíz del repositorio:  docker build -f docker/Dockerfile -t control-impresion .

FROM golang:1.25-alpine AS build
WORKDIR /src
COPY go.mod go.sum ./
COPY internal ./internal
COPY server ./server
COPY src ./src
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/control-server ./server

FROM alpine:3.21
# cups: cupsd; cups-client: lp, lpstat, lpadmin, cancel; ipptool: lectura de atributos de los trabajos.
# Sin cups-filters ni ghostscript: los trabajos se reenvían tal cual (RAW).
RUN apk add --no-cache cups cups-client ipptool \
 && rm -rf /usr/share/cups/doc-root /usr/share/cups/templates /usr/share/cups/locale /usr/share/doc /usr/share/man /usr/lib/cups/cgi-bin /etc/cups/ppd \
 && (getent group lpadmin >/dev/null || addgroup -S lpadmin) \
 && adduser -S -D -H -u 1000 -G lpadmin -s /sbin/nologin control \
 && mkdir -p /data /var/spool/cups/tmp /var/cache/cups /var/log/cups /run/cups \
 && chown -R control:lpadmin /data /etc/cups /var/spool/cups /var/cache/cups /var/log/cups /run/cups
COPY docker/cupsd.conf /etc/cups/cupsd.conf
COPY docker/cups-files.conf /etc/cups/cups-files.conf
COPY --from=build /out/control-server /usr/local/bin/control-server
# Carpeta de la extensión de navegador: el servidor la publica en /ext/version y /ext/package.zip (zip generado al vuelo).
COPY extension /app/extension
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
# Backend que hace esperar cada trabajo el veredicto de control-server (también los enviados con job-hold-until=no-hold).
COPY docker/spoolguard-backend /usr/lib/cups/backend/spoolguard
RUN mkdir -p /var/lib/spoolguard/verdicts && chown control:lpadmin /var/lib/spoolguard /var/lib/spoolguard/verdicts \
 && chmod 0755 /usr/lib/cups/backend/spoolguard \
 && chmod 0755 /usr/local/bin/entrypoint.sh && chown control:lpadmin /etc/cups/cupsd.conf /etc/cups/cups-files.conf

ENV CONTROL_IMPRESION_DIR=/data CONTROL_LISTEN=0.0.0.0:8080 CONTROL_EXT_DIR=/app/extension
USER control
VOLUME /data
EXPOSE 631 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=15s CMD wget -qO- http://127.0.0.1:8080/healthz >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
