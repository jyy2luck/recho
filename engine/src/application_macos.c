// Read-only socket ownership lookup. No privileges, proxy settings or process
// arguments are requested; inaccessible / vanished processes remain unknown.
#include <libproc.h>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double monotonic_seconds(void) {
    struct timespec time;
    clock_gettime(CLOCK_MONOTONIC, &time);
    return time.tv_sec + time.tv_nsec / 1e9;
}

int recho_socket_application(uint32_t client_ip, uint16_t client_port,
                                uint32_t proxy_ip, uint16_t proxy_port,
                                char *path, uint32_t path_size,
                                char *name, uint32_t name_size) {
    // A remote device cannot be attributed to a process on this Mac.
    struct ifaddrs *interfaces = NULL;
    int local_client = 0;
    if (getifaddrs(&interfaces) == 0) {
        for (struct ifaddrs *item = interfaces; item; item = item->ifa_next) {
            if (item->ifa_addr && item->ifa_addr->sa_family == AF_INET &&
                ((struct sockaddr_in *)item->ifa_addr)->sin_addr.s_addr == client_ip) {
                local_client = 1;
                break;
            }
        }
        freeifaddrs(interfaces);
    }
    if (!local_client) return 0;
    int capacity = proc_listallpids(NULL, 0) + 128;
    if (capacity < 1 || capacity > 65536) return 0;
    pid_t *pids = calloc((size_t)capacity, sizeof(pid_t));
    if (!pids) return 0;
    int count = proc_listallpids(pids, capacity * (int)sizeof(pid_t));
    int found = 0;
    // A busy / restricted machine must not stall the actual proxy connection.
    double deadline = monotonic_seconds() + 0.15;
    for (int i = 0; i < count && i < capacity && !found; i++) {
        if (monotonic_seconds() > deadline) break;
        int size = proc_pidinfo(pids[i], PROC_PIDLISTFDS, 0, NULL, 0);
        if (size <= 0 || size > 8 * 1024 * 1024) continue;
        size += 32 * (int)sizeof(struct proc_fdinfo);
        struct proc_fdinfo *fds = malloc((size_t)size);
        if (!fds) break;
        int bytes = proc_pidinfo(pids[i], PROC_PIDLISTFDS, 0, fds, size);
        for (int j = 0; j < bytes / (int)sizeof(*fds); j++) {
            if (monotonic_seconds() > deadline) break;
            if (fds[j].proc_fdtype != PROX_FDTYPE_SOCKET) continue;
            struct socket_fdinfo socket;
            if (proc_pidfdinfo(pids[i], fds[j].proc_fd, PROC_PIDFDSOCKETINFO,
                               &socket, sizeof(socket)) != sizeof(socket)) continue;
            if (socket.psi.soi_kind != SOCKINFO_TCP) continue;
            const struct in_sockinfo *in = &socket.psi.soi_proto.pri_tcp.tcpsi_ini;
            if (!(in->insi_vflag & INI_IPV4) ||
                (uint16_t)in->insi_lport != client_port ||
                (uint16_t)in->insi_fport != proxy_port ||
                in->insi_laddr.ina_46.i46a_addr4.s_addr != client_ip ||
                in->insi_faddr.ina_46.i46a_addr4.s_addr != proxy_ip) continue;
            if (proc_pidpath(pids[i], path, path_size) > 0) {
                proc_name(pids[i], name, name_size);
                path[path_size - 1] = 0;
                name[name_size - 1] = 0;
                found = pids[i];
                break;
            }
        }
        free(fds);
    }
    free(pids);
    return found;
}
