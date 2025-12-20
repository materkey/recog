/**
 * Simple Date Picker Component
 * Adds date filtering functionality to the meetings list
 */

const DatePicker = {
    filterDate: localStorage.getItem('filterDate') || '',
    onChange: null,
    input: null,

    init(inputId, onChange) {
        this.onChange = onChange;
        this.input = document.getElementById(inputId);
        if (!this.input) return;

        // Restore saved date
        this.input.value = this.filterDate;

        // Handle change
        this.input.addEventListener('change', (e) => {
            this.setDate(e.target.value);
        });

        // Clear on double-click
        this.input.addEventListener('dblclick', () => {
            this.setDate('');
        });

        // Navigation buttons
        const prevBtn = document.getElementById('date-prev');
        const nextBtn = document.getElementById('date-next');

        if (prevBtn) {
            prevBtn.addEventListener('click', () => this.navigate(-1));
        }
        if (nextBtn) {
            nextBtn.addEventListener('click', () => this.navigate(1));
        }
    },

    setDate(dateStr) {
        this.filterDate = dateStr;
        this.input.value = dateStr;
        localStorage.setItem('filterDate', dateStr);
        if (this.onChange) this.onChange(dateStr);
    },

    navigate(days) {
        let date;
        if (this.filterDate) {
            date = new Date(this.filterDate);
        } else {
            date = new Date();
        }
        date.setDate(date.getDate() + days);
        const dateStr = date.toISOString().split('T')[0];
        this.setDate(dateStr);
    },

    getDate() {
        return this.filterDate;
    },

    // Filter meetings by date (YYYY-MM-DD format)
    filterMeetings(meetings, dateStr) {
        if (!dateStr) return meetings;

        return meetings.filter(m => {
            const createdAt = m.created_at;
            if (!createdAt) return false;
            // created_at is ISO format, extract date part
            const meetingDate = createdAt.split('T')[0];
            return meetingDate === dateStr;
        });
    }
};

// Export for use in app.js
window.DatePicker = DatePicker;
