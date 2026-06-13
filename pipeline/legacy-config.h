/* Portable configuration for the direct legacy test build.
 *
 * The differential harness needs a reference binary, not an installed
 * autotools package. These facilities are available on supported Linux and
 * macOS CI/development hosts. Keep this list conservative so fdwatch uses poll.
 */
#ifndef THTTPD_RS_LEGACY_CONFIG_H
#define THTTPD_RS_LEGACY_CONFIG_H

#define HAVE_ARPA_INET_H 1
#define HAVE_DIRENT_H 1
#define HAVE_FCNTL_H 1
#define HAVE_GRP_H 1
#define HAVE_MEMORY_H 1
#define HAVE_NETDB_H 1
#define HAVE_NETINET_IN_H 1
#define HAVE_POLL_H 1
#define HAVE_STDLIB_H 1
#define HAVE_STRING_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_SOCKET_H 1
#define HAVE_SYS_TIME_H 1
#define HAVE_SYSLOG_H 1
#define HAVE_UNISTD_H 1

#define HAVE_ATOLL 1
#define HAVE_BZERO 1
#define HAVE_CLOCK_GETTIME 1
#define HAVE_DAEMON 1
#define HAVE_DUP2 1
#define HAVE_GAI_STRERROR 1
#define HAVE_GETADDRINFO 1
#define HAVE_GETCWD 1
#define HAVE_GETHOSTBYNAME 1
#define HAVE_GETHOSTNAME 1
#define HAVE_GETNAMEINFO 1
#define HAVE_GETPASS 1
#define HAVE_GETTIMEOFDAY 1
#define HAVE_INET_NTOA 1
#define HAVE_MEMMOVE 1
#define HAVE_MEMSET 1
#define HAVE_MKDIR 1
#define HAVE_MMAP 1
#define HAVE_MUNMAP 1
#define HAVE_POLL 1
#define HAVE_SELECT 1
#define HAVE_SETSID 1
#define HAVE_SOCKET 1
#define HAVE_STRCASECMP 1
#define HAVE_STRCHR 1
#define HAVE_STRCSPN 1
#define HAVE_STRDUP 1
#define HAVE_STRERROR 1
#define HAVE_STRNCASECMP 1
#define HAVE_STRPBRK 1
#define HAVE_STRRCHR 1
#define HAVE_STRSPN 1
#define HAVE_STRSTR 1
#define HAVE_TZSET 1
#define HAVE_VSNPRINTF 1
#define HAVE_WAITPID 1

#define STDC_HEADERS 1
#define TIME_WITH_SYS_TIME 1

#endif
