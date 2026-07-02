/*
 * htpasswd.c: simple program for manipulating password file for NCSA httpd
 * 
 * Rob McCool
 */

/* Modified 29aug97 by Jef Poskanzer to accept new password on stdin,
** if stdin is a pipe or file.  This is necessary for use from CGI.
*/

#include <config.h>

//system headers
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

extern char *crypt(const char *key, const char *setting);

#define LF 10
#define CR 13

#define MAX_STRING_LEN 256

int tfd;
/* Temp path is built per-run adjacent to the destination (see main()) so
** the final replace is an atomic rename() on one filesystem — no shell. */
char temp_template[MAX_STRING_LEN + 32];

void interrupted(int);

static char * strd(char *s) {
    char *d;

    d=(char *)malloc(strlen(s) + 1);
    strcpy(d,s);
    return(d);
}

static void getword(char *word, char *line, char stop) {
    int x = 0,y;

    for(x=0;((line[x]) && (line[x] != stop));x++)
	word[x] = line[x];

    word[x] = '\0';
    if(line[x]) ++x;
    y=0;

    while((line[y++] = line[x++]));
}

static int get_line(char *s, int n, FILE *f) {
    register int i=0;

    while(1) {
	s[i] = (char)fgetc(f);

	if(s[i] == CR)
	    s[i] = fgetc(f);

	if((s[i] == 0x4) || (s[i] == LF) || (i == (n-1))) {
	    s[i] = '\0';
	    return (feof(f) ? 1 : 0);
	}
	++i;
    }
}

static void putline(FILE *f,char *l) {
    int x;

    for(x=0;l[x];x++) fputc(l[x],f);
    fputc('\n',f);
}


/* From local_passwd.c (C) Regents of Univ. of California blah blah */
static unsigned char itoa64[] =         /* 0 ... 63 => ascii - 64 */
	"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static void to64(register char *s, register long v, register int n) {
    while (--n >= 0) {
	*s++ = itoa64[v&0x3f];
	v >>= 6;
    }
}

#ifdef MPE
/* MPE lacks getpass() and a way to suppress stdin echo.  So for now, just
issue the prompt and read the results with echo.  (Ugh). */

char *getpass(const char *prompt) {

static char password[81];

fputs(prompt,stderr);
gets((char *)&password);

if (strlen((char *)&password) > 8) {
  password[8]='\0';
}

return (char *)&password;
}
#endif

static void
add_password( char* user, FILE* f )
    {
    char pass[100];
    char* pw;
    char* cpw;
    char salt[3];

    if ( ! isatty( fileno( stdin ) ) )
	{
	(void) fgets( pass, sizeof(pass), stdin );
	if ( pass[strlen(pass) - 1] == '\n' )
	    pass[strlen(pass) - 1] = '\0';
	pw = pass;
	}
    else
	{
	pw = strd( (char*) getpass( "New password:" ) );
	if ( strcmp( pw, (char*) getpass( "Re-type new password:" ) ) != 0 )
	    {
	    (void) fprintf( stderr, "They don't match, sorry.\n" );
	    if ( tfd != -1 )
		unlink( temp_template );
	    exit( 1 );
	    }
	}
    (void) srandom( (int) time( (time_t*) 0 ) );
    to64( &salt[0], random(), 2 );
    cpw = crypt( pw, salt );
    if (cpw)
	(void) fprintf( f, "%s:%s\n", user, cpw );
    else
	(void) fprintf( stderr, "crypt() returned NULL, sorry\n" );
    }

static void usage(void) {
    fprintf(stderr,"Usage: htpasswd [-c] passwordfile username\n");
    fprintf(stderr,"The -c flag creates a new file.\n");
    exit(1);
}

void interrupted(int signo) {
    fprintf(stderr,"Interrupted.\n");
    if(tfd != -1) unlink(temp_template);
    exit(1);
}

int main(int argc, char *argv[]) {
    FILE *tfp,*f;
    char user[MAX_STRING_LEN];
    char pwfilename[MAX_STRING_LEN];
    char line[MAX_STRING_LEN];
    char l[MAX_STRING_LEN];
    char w[MAX_STRING_LEN];
    int found;
    struct stat pwstat;
    struct stat tempstat;

    tfd = -1;
    signal(SIGINT,(void (*)(int))interrupted);
    if(argc == 4) {
	if(strcmp(argv[1],"-c"))
	    usage();
	if(!(tfp = fopen(argv[2],"w"))) {
	    fprintf(stderr,"Could not open passwd file %s for writing.\n",
		    argv[2]);
	    perror("fopen");
	    exit(1);
	}
	if (strlen(argv[2]) > (sizeof(pwfilename) - 1)) {
	    fprintf(stderr, "%s: filename is too long\n", argv[0]);
	    exit(1);
	}
	/* No shell is used anywhere in htpasswd (the update path renames a
	** temp file), so filename characters like spaces, ';' or '>' are safe
	** and no longer rejected. */
	if (strlen(argv[3]) > (sizeof(user) - 1)) {
	    fprintf(stderr, "%s: username is too long\n", argv[0]);
	    exit(1);
	}
	if ((strchr(argv[3], ':')) != NULL) {
	    fprintf(stderr, "%s: username contains an illegal character\n",
		argv[0]);
	    exit(1);
	}
	printf("Adding password for %s.\n",argv[3]);
	add_password(argv[3],tfp);
	fclose(tfp);
	exit(0);
    } else if(argc != 3) usage();

    /* Build the temp path ADJACENT to the destination so the final move is
    ** an atomic rename() on one filesystem — no shell, no cp, and no
    ** cross-filesystem copy.  The old code used system("cp ...") which
    ** allowed shell injection via the destination path. */
    if (strlen(argv[1]) > (sizeof(pwfilename) - 1)) {
	fprintf(stderr, "%s: filename is too long\n", argv[0]);
	exit(1);
    }
    if (strlen(argv[2]) > (sizeof(user) - 1)) {
	fprintf(stderr, "%s: username is too long\n", argv[0]);
	exit(1);
    }
    if ((strchr(argv[2], ':')) != NULL) {
	fprintf(stderr, "%s: username contains an illegal character\n",
		argv[0]);
	exit(1);
    }
    if(stat(argv[1],&pwstat) != 0) {
	fprintf(stderr,
		"Could not stat passwd file %s.\n",argv[1]);
	fprintf(stderr,"Use -c option to create new one.\n");
	perror("stat");
	exit(1);
    }
    snprintf(temp_template, sizeof(temp_template), "%s.tmpXXXXXX", argv[1]);
    tfd = mkstemp(temp_template);
    if(tfd == -1) {
	fprintf(stderr,"Could not create temp file.\n");
	perror("mkstemp");
	exit(1);
    }
    if(!(tfp = fdopen(tfd,"w"))) {
	fprintf(stderr,"Could not open temp file.\n");
	perror("fdopen");
	close(tfd);
	unlink(temp_template);
	exit(1);
    }

    if(!(f = fopen(argv[1],"r"))) {
	fprintf(stderr,
		"Could not open passwd file %s for reading.\n",argv[1]);
	fprintf(stderr,"Use -c option to create new one.\n");
	fclose(tfp);
	unlink(temp_template);
	exit(1);
    }
    strcpy(user,argv[2]);

    found = 0;
    while(!(get_line(line,MAX_STRING_LEN,f))) {
	if(found || (line[0] == '#') || (!line[0])) {
	    putline(tfp,line);
	    continue;
	}
	strcpy(l,line);
	getword(w,l,':');
	if(strcmp(user,w)) {
	    putline(tfp,line);
	    continue;
	}
	else {
	    printf("Changing password for user %s\n",user);
	    add_password(user,tfp);
	    found = 1;
	}
    }
    if(!found) {
	printf("Adding user %s\n",user);
	add_password(user,tfp);
    }
    fclose(f);
    if(fflush(tfp) != 0) {
	perror("fflush");
	fclose(tfp);
	unlink(temp_template);
	exit(1);
    }
    if(fstat(tfd,&tempstat) != 0) {
	perror("fstat");
	fclose(tfp);
	unlink(temp_template);
	exit(1);
    }
    if((tempstat.st_uid != pwstat.st_uid || tempstat.st_gid != pwstat.st_gid) &&
       fchown(tfd,pwstat.st_uid,pwstat.st_gid) != 0) {
	perror("fchown");
	fclose(tfp);
	unlink(temp_template);
	exit(1);
    }
    if(fchmod(tfd,pwstat.st_mode & 07777) != 0) {
	perror("fchmod");
	fclose(tfp);
	unlink(temp_template);
	exit(1);
    }
    if(fclose(tfp) != 0) {
	perror("fclose");
	unlink(temp_template);
	exit(1);
    }
    /* Atomically replace the destination with the completed temp file.
    ** rename() is POSIX-atomic and invokes no shell, so the destination
    ** path may safely contain spaces or shell metacharacters. */
    if(rename(temp_template,argv[1]) != 0) {
	perror("rename");
	unlink(temp_template);
	exit(1);
    }
    exit(0);
}
